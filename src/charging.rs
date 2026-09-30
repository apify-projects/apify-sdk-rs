//! Pay-per-event charging: [`ChargingManager`] keeps track of what the run has charged and what
//! its budget (`ACTOR_MAX_TOTAL_CHARGE_USD`) still allows, a port of the JS SDK's.
//!
//! Items pushed to the default dataset are charged as the synthetic `apify-default-dataset-item`
//! event, by the storage backend at the moment they are stored.

use std::sync::Arc;

use apify_client::{ApifyClient, ApifyClientError, RunChargeOptions};
use async_trait::async_trait;
use crawlee::core::StorageResult;
use crawlee::core::errors::StorageError;
use crawlee::core::storage::backend::{
    DatasetBackend, DatasetInfo, DatasetItem, DatasetListOptions, KeyValueStoreBackend, PaginatedList,
    RequestQueueBackend, StorageBackend, StorageKind,
};
use crawlee::{Dataset, StorageIdentifier};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::Value;

use crate::configuration::Configuration;
use crate::storage::DEFAULT_STORAGE_ALIAS;

/// The synthetic event of an item stored in the default dataset.
pub const DEFAULT_DATASET_ITEM_EVENT: &str = "apify-default-dataset-item";
const LOCAL_CHARGING_LOG_DATASET_NAME: &str = "charging_log";
const PAY_PER_EVENT: &str = "PAY_PER_EVENT";

/// `Number(x.toFixed(digits))` of JavaScript: rounds the exact value of `x`, halves away from zero.
pub(crate) fn js_to_fixed(x: f64, digits: usize) -> f64 {
    if !x.is_finite() || x.abs() >= 1e21 {
        return x;
    }
    // The exact decimal expansion of an f64 has at most 1074 fractional digits.
    let exact = format!("{:.1074}", x.abs());
    let (int_part, frac) = exact.split_once('.').expect("a fractional part");
    let mut digits_str: Vec<u8> = int_part.bytes().chain(frac.bytes().take(digits)).collect();
    if frac.as_bytes()[digits] >= b'5' {
        // Increment the decimal number in `digits_str`.
        let mut i = digits_str.len();
        loop {
            if i == 0 {
                digits_str.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits_str[i] == b'9' {
                digits_str[i] = b'0';
            } else {
                digits_str[i] += 1;
                break;
            }
        }
    }
    let split = digits_str.len() - digits;
    let text = format!(
        "{}{}.{}",
        if x < 0.0 { "-" } else { "" },
        std::str::from_utf8(&digits_str[..split]).expect("digits"),
        std::str::from_utf8(&digits_str[split..]).expect("digits")
    );
    text.parse().expect("a number")
}

/// A charge to make.
#[derive(Clone, Debug)]
pub struct ChargeOptions {
    pub event_name: String,
    pub count: u64,
    pub idempotency_key: Option<String>,
}

impl ChargeOptions {
    pub fn new(event_name: impl Into<String>, count: u64) -> Self {
        ChargeOptions { event_name: event_name.into(), count, idempotency_key: None }
    }
}

/// What a charge did. Counts of `None` mean no limit.
#[derive(Clone, Debug, PartialEq)]
pub struct ChargeResult {
    /// The budget allows no more events of this kind.
    pub event_charge_limit_reached: bool,
    pub charged_count: u64,
    /// How many more events of each priced kind the budget allows.
    pub chargeable_within_limit: IndexMap<String, Option<u64>>,
}

/// The pricing of the run.
#[derive(Clone, Debug, PartialEq)]
pub struct ActorPricingInfo {
    pub pricing_model: Option<String>,
    pub is_pay_per_event: bool,
    pub max_total_charge_usd: f64,
    pub per_event_prices: IndexMap<String, f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum ChargingError {
    #[error("ChargingManager is not initialized")]
    NotInitialized,
    #[error("Cannot charge for synthetic event '{0}' manually")]
    SyntheticEvent(String),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Api(#[from] ApifyClientError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("invalid pricing information: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
struct EventPrice {
    price: f64,
    title: String,
}

#[derive(Default)]
struct State {
    initialized: bool,
    pricing_model: Option<String>,
    max_total_charge_usd: f64,
    prices: IndexMap<String, EventPrice>,
    /// Charged count and amount by event.
    charged: IndexMap<String, (u64, f64)>,
    not_ppe_warning_printed: bool,
}

/// `f64` counts (possibly infinite) as optional integers.
fn limit(count: f64) -> Option<u64> {
    count.is_finite().then(|| count.max(0.0) as u64)
}

tokio::task_local! {
    /// Set inside [`ChargingManager::with_charge_lock`], which is re-entrant within a task.
    static CHARGE_LOCK_HELD: ();
}

/// Tracks the charges of a run and makes them.
pub struct ChargingManager {
    configuration: Arc<Configuration>,
    client: ApifyClient,
    state: Mutex<State>,
    lock: tokio::sync::Mutex<()>,
    log_dataset: tokio::sync::OnceCell<Dataset>,
}

impl std::fmt::Debug for ChargingManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChargingManager").finish_non_exhaustive()
    }
}

impl ChargingManager {
    pub fn new(configuration: Arc<Configuration>, client: ApifyClient) -> Self {
        let max_total_charge_usd = configuration.max_total_charge_usd;
        ChargingManager {
            configuration,
            client,
            state: Mutex::new(State { max_total_charge_usd, ..State::default() }),
            lock: tokio::sync::Mutex::new(()),
            log_dataset: tokio::sync::OnceCell::new(),
        }
    }

    /// Loads the pricing and what was charged so far (by an earlier incarnation of the run), from
    /// the environment or the API. `storage` holds the local charging log dataset.
    pub async fn init(&self, storage: &dyn StorageBackend) -> Result<(), ChargingError> {
        let configuration = &self.configuration;
        if configuration.use_charging_log_dataset && configuration.is_at_home {
            return Err(ChargingError::Invalid(
                "Using the ACTOR_USE_CHARGING_LOG_DATASET environment variable is only supported in a local \
                 development environment"
                    .to_owned(),
            ));
        }
        if configuration.test_pay_per_event && configuration.is_at_home {
            return Err(ChargingError::Invalid(
                "Using the ACTOR_TEST_PAY_PER_EVENT environment variable is only supported in a local development \
                 environment"
                    .to_owned(),
            ));
        }

        let (pricing_info, charged_counts, max_total_charge_usd) = self.fetch_pricing_info().await?;
        let is_ppe = {
            let mut state = self.state.lock();
            let run_model = pricing_info.get("pricingModel").and_then(Value::as_str).map(str::to_owned);
            state.pricing_model =
                if configuration.test_pay_per_event { Some(PAY_PER_EVENT.to_owned()) } else { run_model.clone() };
            if run_model.as_deref() == Some(PAY_PER_EVENT) {
                let events = pricing_info.pointer("/pricingPerEvent/actorChargeEvents").and_then(Value::as_object);
                for (name, pricing) in events.into_iter().flatten() {
                    let price = pricing.get("eventPriceUsd").and_then(Value::as_f64).unwrap_or(0.0);
                    let title = pricing.get("eventTitle").and_then(Value::as_str).unwrap_or_default().to_owned();
                    state.prices.insert(name.clone(), EventPrice { price, title });
                }
                state.max_total_charge_usd = max_total_charge_usd;
            }
            state.charged.clear();
            for (name, count) in charged_counts {
                let price = state.prices.get(&name).map_or(0.0, |p| p.price);
                state.charged.insert(name, (count, count as f64 * price));
            }
            state.initialized = true;
            state.pricing_model.as_deref() == Some(PAY_PER_EVENT)
        };

        if is_ppe && configuration.use_charging_log_dataset {
            let name = StorageIdentifier::name(LOCAL_CHARGING_LOG_DATASET_NAME);
            if configuration.crawlee.purge_on_start {
                Dataset::open(storage, &name).await?.drop_storage().await?;
            }
            let dataset = Dataset::open(storage, &name).await?;
            let _ = self.log_dataset.set(dataset);
        }
        Ok(())
    }

    /// Pricing info (JSON), charged counts, and the budget.
    async fn fetch_pricing_info(&self) -> Result<(Value, IndexMap<String, u64>, f64), ChargingError> {
        let configuration = &self.configuration;
        let counts = |value: Option<&Value>| -> IndexMap<String, u64> {
            value
                .and_then(Value::as_object)
                .map(|counts| counts.iter().filter_map(|(k, v)| Some((k.clone(), v.as_u64()?))).collect())
                .unwrap_or_default()
        };
        if let (Some(pricing), Some(charged)) = (&configuration.actor_pricing_info, &configuration.charged_event_counts)
        {
            let pricing: Value = serde_json::from_str(pricing)?;
            let charged: Value = serde_json::from_str(charged)?;
            return Ok((pricing, counts(Some(&charged)), configuration.max_total_charge_usd));
        }
        if configuration.is_at_home {
            let run_id = configuration.actor_run_id.clone().ok_or_else(|| {
                ChargingError::Invalid("Actor run ID not found even though the Actor is running on Apify".to_owned())
            })?;
            let run = self
                .client
                .run(run_id)
                .get()
                .await?
                .ok_or_else(|| ChargingError::Invalid("Actor run not found".to_owned()))?;
            let max = run
                .extra
                .get("options")
                .and_then(|options| options.get("maxTotalChargeUsd"))
                .and_then(Value::as_f64)
                .filter(|&max| max != 0.0)
                .unwrap_or(f64::INFINITY);
            let pricing = run.extra.get("pricingInfo").cloned().unwrap_or(Value::Null);
            return Ok((pricing, counts(run.extra.get("chargedEventCounts")), max));
        }
        Ok((Value::Null, IndexMap::new(), configuration.max_total_charge_usd))
    }

    pub fn is_initialized(&self) -> bool {
        self.state.lock().initialized
    }

    pub fn is_pay_per_event(&self) -> bool {
        self.state.lock().pricing_model.as_deref() == Some(PAY_PER_EVENT)
    }

    pub fn pricing_info(&self) -> Result<ActorPricingInfo, ChargingError> {
        let state = self.state.lock();
        if !state.initialized {
            return Err(ChargingError::NotInitialized);
        }
        Ok(ActorPricingInfo {
            is_pay_per_event: state.pricing_model.as_deref() == Some(PAY_PER_EVENT),
            pricing_model: state.pricing_model.clone(),
            max_total_charge_usd: state.max_total_charge_usd,
            per_event_prices: state.prices.iter().map(|(name, p)| (name.clone(), p.price)).collect(),
        })
    }

    pub fn charged_event_count(&self, event_name: &str) -> u64 {
        self.state.lock().charged.get(event_name).map_or(0, |(count, _)| *count)
    }

    /// Runs `f` holding the charge lock (when pay-per-event), so that a reservation and the charge
    /// acting on it are not interleaved with other charges. Re-entrant within a task.
    pub async fn with_charge_lock<F: Future>(&self, f: F) -> F::Output {
        if !self.is_pay_per_event() || CHARGE_LOCK_HELD.try_with(|_| ()).is_ok() {
            return f.await;
        }
        let _guard = self.lock.lock().await;
        CHARGE_LOCK_HELD.scope((), f).await
    }

    /// Charges for `count` events, as far as the budget allows. When it allows fewer, one more is
    /// charged, so that the platform notices and stops the run.
    pub async fn charge(&self, options: ChargeOptions) -> Result<ChargeResult, ChargingError> {
        if !self.is_pay_per_event() {
            let mut state = self.state.lock();
            if !state.not_ppe_warning_printed {
                tracing::warn!(
                    "Ignored attempt to charge for an event - the Actor does not use the pay-per-event pricing"
                );
                state.not_ppe_warning_printed = true;
            }
            return Ok(ChargeResult {
                event_charge_limit_reached: false,
                charged_count: 0,
                chargeable_within_limit: self.chargeable_within_limit_of(&state),
            });
        }
        if !self.is_initialized() {
            return Err(ChargingError::NotInitialized);
        }
        self.with_charge_lock(self.charge_locked(options)).await
    }

    async fn charge_locked(&self, options: ChargeOptions) -> Result<ChargeResult, ChargingError> {
        let ChargeOptions { event_name, count, idempotency_key } = options;
        let is_at_home = self.configuration.is_at_home;
        let (charged_count, pricing) = {
            let mut state = self.state.lock();
            let max = self.max_event_charge_count_of(&state, &event_name);
            let charged_count = if (count as f64) <= max {
                count
            } else if self.total_charged_amount_of(&state) <= state.max_total_charge_usd {
                // Over budget: one more than allowed, so that the platform stops the run. Not when
                // already strictly over it.
                max as u64 + 1
            } else {
                0
            };
            if charged_count == 0 {
                return Ok(ChargeResult {
                    event_charge_limit_reached: count > 0,
                    charged_count: 0,
                    chargeable_within_limit: self.chargeable_within_limit_of(&state),
                });
            }
            // Unknown events cost 1 locally, so that the budget can be reached in development.
            let pricing = state.prices.get(&event_name).cloned().unwrap_or_else(|| EventPrice {
                price: if is_at_home { 0.0 } else { 1.0 },
                title: format!("Unknown event '{event_name}'"),
            });
            let entry = state.charged.entry(event_name.clone()).or_insert((0, 0.0));
            entry.0 += charged_count;
            entry.1 += charged_count as f64 * pricing.price;
            (charged_count, pricing)
        };

        if is_at_home {
            let known = self.state.lock().prices.contains_key(&event_name);
            if event_name.starts_with("apify-") {
                // Synthetic events are charged by the platform itself, from the dataset writes.
            } else if known {
                let run_id = self.configuration.actor_run_id.clone().unwrap_or_default();
                let charge = RunChargeOptions {
                    event_name: event_name.clone(),
                    count: Some(charged_count as i64),
                    idempotency_key,
                };
                self.client.run(run_id).charge(charge).await?;
            } else {
                tracing::warn!("Attempting to charge for an unknown event '{event_name}'");
            }
        }
        if let Some(dataset) = self.log_dataset.get() {
            dataset
                .push_data(&serde_json::json!({
                    "eventName": event_name,
                    "eventTitle": pricing.title,
                    "eventPriceUsd": pricing.price,
                    "chargedCount": charged_count,
                    "timestamp": crawlee::core::now_iso(),
                }))
                .await?;
        }
        if charged_count < count {
            let subject = if count == 1 { "instance" } else { "instances" };
            tracing::info!(
                "Charging {count} {subject} of '{event_name}' event would exceed maxTotalChargeUsd - only \
                 {charged_count} events were charged"
            );
        }
        let state = self.state.lock();
        Ok(ChargeResult {
            event_charge_limit_reached: self.is_event_charge_limit_reached_of(&state, &event_name),
            charged_count,
            chargeable_within_limit: self.chargeable_within_limit_of(&state),
        })
    }

    fn total_charged_amount_of(&self, state: &State) -> f64 {
        js_to_fixed(state.charged.values().map(|(_, amount)| amount).sum(), 6)
    }

    /// The price used for limits: the real one on the platform, 1 locally.
    fn event_price_of(&self, state: &State, event_name: &str) -> Option<f64> {
        if self.configuration.is_at_home { state.prices.get(event_name).map(|p| p.price) } else { Some(1.0) }
    }

    fn max_charges_by_price_of(&self, state: &State, price: f64) -> f64 {
        let unrounded = (state.max_total_charge_usd - self.total_charged_amount_of(state)) / price;
        // Rounded first: 4.9999999999999999 must not floor to 4.
        js_to_fixed(unrounded, 4).floor().max(0.0)
    }

    fn max_event_charge_count_of(&self, state: &State, event_name: &str) -> f64 {
        match self.event_price_of(state, event_name) {
            Some(price) if price != 0.0 => self.max_charges_by_price_of(state, price),
            _ => f64::INFINITY,
        }
    }

    fn is_event_charge_limit_reached_of(&self, state: &State, event_name: &str) -> bool {
        state.pricing_model.as_deref() == Some(PAY_PER_EVENT)
            && self.max_event_charge_count_of(state, event_name) <= 0.0
    }

    fn chargeable_within_limit_of(&self, state: &State) -> IndexMap<String, Option<u64>> {
        state.prices.keys().map(|name| (name.clone(), limit(self.max_event_charge_count_of(state, name)))).collect()
    }

    /// How many more `event_name` events the budget allows; `None` is no limit.
    pub fn max_event_charge_count_within_limit(&self, event_name: &str) -> Option<u64> {
        limit(self.max_event_charge_count_of(&self.state.lock(), event_name))
    }

    pub fn is_event_charge_limit_reached(&self, event_name: &str) -> bool {
        self.is_event_charge_limit_reached_of(&self.state.lock(), event_name)
    }

    pub fn chargeable_within_limit(&self) -> IndexMap<String, Option<u64>> {
        self.chargeable_within_limit_of(&self.state.lock())
    }

    /// How many of `items_count` items can be pushed within the budget, each charged as
    /// `event_name` (if set) plus the default dataset item event (if `is_default_dataset`).
    pub fn push_data_limit(&self, items_count: usize, event_name: Option<&str>, is_default_dataset: bool) -> usize {
        let state = self.state.lock();
        if state.pricing_model.as_deref() != Some(PAY_PER_EVENT) || items_count == 0 {
            return items_count;
        }
        let item_price = event_name.map_or(0.0, |name| self.event_price_of(&state, name).unwrap_or(0.0))
            + if is_default_dataset {
                self.event_price_of(&state, DEFAULT_DATASET_ITEM_EVENT).unwrap_or(0.0)
            } else {
                0.0
            };
        if item_price == 0.0 {
            return items_count;
        }
        let max = self.max_charges_by_price_of(&state, item_price);
        if max >= items_count as f64 {
            return items_count;
        }
        if max > 0.0 {
            return max as usize;
        }
        usize::from(self.total_charged_amount_of(&state) <= state.max_total_charge_usd)
    }
}

/// The default dataset, charging the default dataset item event for what it stores.
struct ChargingDataset {
    inner: Arc<dyn DatasetBackend>,
    charging: Arc<ChargingManager>,
}

#[async_trait]
impl DatasetBackend for ChargingDataset {
    async fn get_metadata(&self) -> StorageResult<DatasetInfo> {
        self.inner.get_metadata().await
    }

    async fn drop_storage(&self) -> StorageResult<()> {
        self.inner.drop_storage().await
    }

    async fn purge(&self) -> StorageResult<()> {
        self.inner.purge().await
    }

    async fn push_data(&self, mut items: Vec<DatasetItem>) -> StorageResult<()> {
        let charging = &self.charging;
        // Datasets also work before the Actor is initialized, with nothing to charge.
        if items.is_empty() || !charging.is_initialized() || !charging.is_pay_per_event() {
            return self.inner.push_data(items).await;
        }
        charging
            .with_charge_lock(async {
                let limit = charging.push_data_limit(items.len(), None, true);
                if limit == 0 {
                    return Ok(());
                }
                items.truncate(limit);
                self.inner.push_data(items).await?;
                charging
                    .charge(ChargeOptions::new(DEFAULT_DATASET_ITEM_EVENT, limit as u64))
                    .await
                    .map_err(|err| StorageError::Backend(Box::new(err)))?;
                Ok(())
            })
            .await
    }

    async fn get_data(&self, options: DatasetListOptions) -> StorageResult<PaginatedList<DatasetItem>> {
        self.inner.get_data(options).await
    }
}

/// A storage backend whose default dataset charges for its items.
pub(crate) struct ChargingStorageBackend {
    pub(crate) inner: Arc<dyn StorageBackend>,
    pub(crate) charging: Arc<ChargingManager>,
    pub(crate) default_dataset_id: String,
}

impl ChargingStorageBackend {
    fn is_default_dataset(&self, id: &StorageIdentifier) -> bool {
        match id {
            StorageIdentifier::Default => true,
            StorageIdentifier::Alias(alias) => alias == DEFAULT_STORAGE_ALIAS,
            StorageIdentifier::Id(id) => *id == self.default_dataset_id,
            StorageIdentifier::Name(_) => false,
        }
    }
}

#[async_trait]
impl StorageBackend for ChargingStorageBackend {
    async fn create_dataset_backend(&self, id: &StorageIdentifier) -> StorageResult<Arc<dyn DatasetBackend>> {
        let backend = self.inner.create_dataset_backend(id).await?;
        if !self.is_default_dataset(id) {
            return Ok(backend);
        }
        Ok(Arc::new(ChargingDataset { inner: backend, charging: self.charging.clone() }))
    }

    async fn create_key_value_store_backend(
        &self,
        id: &StorageIdentifier,
    ) -> StorageResult<Arc<dyn KeyValueStoreBackend>> {
        self.inner.create_key_value_store_backend(id).await
    }

    async fn create_request_queue_backend(
        &self,
        id: &StorageIdentifier,
    ) -> StorageResult<Arc<dyn RequestQueueBackend>> {
        self.inner.create_request_queue_backend(id).await
    }

    async fn storage_exists(&self, id: &str, kind: StorageKind) -> StorageResult<bool> {
        self.inner.storage_exists(id, kind).await
    }

    async fn purge(&self) -> StorageResult<()> {
        self.inner.purge().await
    }

    async fn teardown(&self) -> StorageResult<()> {
        self.inner.teardown().await
    }

    fn rate_limit_errors(&self) -> u64 {
        self.inner.rate_limit_errors()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_fixed_rounds_like_javascript() {
        // Values checked with node: Number(x.toFixed(n)).
        assert_eq!(js_to_fixed(0.25, 1), 0.3, "a tie rounds up, not to even");
        assert_eq!(js_to_fixed(1.005, 2), 1.0, "1.005 is below 1.005 in binary");
        assert_eq!(js_to_fixed(4.99999999999, 4), 5.0);
        assert_eq!(js_to_fixed(-2.5, 0), -3.0);
        assert_eq!(js_to_fixed(0.1 + 0.2, 6), 0.3);
        assert_eq!(js_to_fixed(9.9999995, 6), 9.999999);
        assert_eq!(js_to_fixed(f64::INFINITY, 4), f64::INFINITY);
    }

    fn manager(max: f64, prices: &[(&str, f64)], charged: &[(&str, u64)]) -> ChargingManager {
        let events: serde_json::Map<String, Value> = prices
            .iter()
            .map(|(name, price)| {
                ((*name).to_owned(), serde_json::json!({ "eventTitle": name, "eventPriceUsd": price }))
            })
            .collect();
        let pricing =
            serde_json::json!({ "pricingModel": "PAY_PER_EVENT", "pricingPerEvent": { "actorChargeEvents": events } });
        let charged: serde_json::Map<String, Value> =
            charged.iter().map(|(name, count)| ((*name).to_owned(), serde_json::json!(count))).collect();
        let configuration = Configuration {
            is_at_home: true,
            actor_run_id: Some("run".to_owned()),
            max_total_charge_usd: max,
            actor_pricing_info: Some(pricing.to_string()),
            charged_event_counts: Some(Value::Object(charged).to_string()),
            ..Configuration::default()
        };
        let client = crate::new_client(&configuration, None, Default::default());
        ChargingManager::new(Arc::new(configuration), client)
    }

    #[tokio::test]
    async fn budget_limits_and_restored_counts() {
        let manager = manager(10.0, &[("page", 1.0), ("apify-default-dataset-item", 0.5)], &[("page", 3)]);
        manager.init(&crawlee::core::MemoryStorageBackend::new()).await.unwrap();
        assert!(manager.is_pay_per_event());
        assert_eq!(manager.charged_event_count("page"), 3);
        assert_eq!(manager.max_event_charge_count_within_limit("page"), Some(7));
        assert_eq!(manager.max_event_charge_count_within_limit("unknown"), None, "unpriced events are unlimited");
        // An item costs 1 + 0.5; 7 USD are left.
        assert_eq!(manager.push_data_limit(10, Some("page"), true), 4);
        assert_eq!(manager.push_data_limit(3, Some("page"), true), 3);

        // Charges above the budget are cut, plus one to make the platform stop the run.
        let result = manager.charge_locked(ChargeOptions::new("apify-default-dataset-item", 20)).await.unwrap();
        assert_eq!(result.charged_count, 15);
        assert!(result.event_charge_limit_reached);
        assert_eq!(manager.max_event_charge_count_within_limit("page"), Some(0));
        let result = manager.charge_locked(ChargeOptions::new("apify-default-dataset-item", 1)).await.unwrap();
        assert_eq!(result.charged_count, 0, "strictly over the budget: nothing more");
        assert!(result.event_charge_limit_reached);
        assert_eq!(manager.push_data_limit(5, None, true), 0);
    }
}
