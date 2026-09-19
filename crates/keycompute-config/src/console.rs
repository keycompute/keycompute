//! Console resource protection is separate from billable model quotas.
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ConsoleConfig {
    pub read_rpm: u32,
    pub heavy_read_rpm: u32,
    pub write_rpm: u32,
    pub user_rpm: u32,
    pub tenant_rpm: u32,
    pub aggregate_rpm: u32,
    pub read_concurrency: usize,
    pub write_concurrency: usize,
    pub origin_concurrency: usize,
    pub queue_limit: usize,
    pub queue_timeout_ms: u64,
}
impl Default for ConsoleConfig {
    fn default() -> Self {
        Self {
            read_rpm: 240,
            heavy_read_rpm: 60,
            write_rpm: 30,
            user_rpm: 300,
            tenant_rpm: 1200,
            aggregate_rpm: 6000,
            read_concurrency: 4,
            write_concurrency: 2,
            origin_concurrency: 2,
            queue_limit: 16,
            queue_timeout_ms: 250,
        }
    }
}
impl ConsoleConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if [
            self.read_rpm,
            self.heavy_read_rpm,
            self.write_rpm,
            self.user_rpm,
            self.tenant_rpm,
            self.aggregate_rpm,
        ]
        .iter()
        .any(|n| !(1..=1_000_000).contains(n))
        {
            return Err("console RPM budgets must be within 1..=1000000");
        }
        if [
            self.read_concurrency,
            self.write_concurrency,
            self.origin_concurrency,
        ]
        .iter()
        .any(|n| !(1..=1024).contains(n))
            || self.origin_concurrency > self.read_concurrency
            || self.queue_limit > 4096
            || !(1..=5000).contains(&self.queue_timeout_ms)
        {
            return Err("invalid console concurrency, queue or wait budget");
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budgets_are_finite_and_validated() {
        assert!(ConsoleConfig::default().validate().is_ok());
        assert!(
            ConsoleConfig {
                read_rpm: 0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ConsoleConfig {
                queue_limit: 4097,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ConsoleConfig {
                origin_concurrency: 5,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
