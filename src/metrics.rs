use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct Metrics {
    records_requests: AtomicU64,
    records_errors: AtomicU64,
    changes_requests: AtomicU64,
    changes_errors: AtomicU64,
    adjust_requests: AtomicU64,
    adjust_errors: AtomicU64,
}

impl Metrics {
    pub fn inc_records_requests(&self) {
        self.records_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_records_errors(&self) {
        self.records_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_changes_requests(&self) {
        self.changes_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_changes_errors(&self) {
        self.changes_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_adjust_requests(&self) {
        self.adjust_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_adjust_errors(&self) {
        self.adjust_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn render(&self) -> String {
        format!(
            "# HELP timeweb_webhook_requests_total Number of webhook requests.\n# TYPE timeweb_webhook_requests_total counter\ntimeweb_webhook_requests_total{{operation=\"records\"}} {}\ntimeweb_webhook_requests_total{{operation=\"changes\"}} {}\ntimeweb_webhook_requests_total{{operation=\"adjustendpoints\"}} {}\n# HELP timeweb_webhook_errors_total Number of webhook errors.\n# TYPE timeweb_webhook_errors_total counter\ntimeweb_webhook_errors_total{{operation=\"records\"}} {}\ntimeweb_webhook_errors_total{{operation=\"changes\"}} {}\ntimeweb_webhook_errors_total{{operation=\"adjustendpoints\"}} {}\n",
            self.records_requests.load(Ordering::Relaxed),
            self.changes_requests.load(Ordering::Relaxed),
            self.adjust_requests.load(Ordering::Relaxed),
            self.records_errors.load(Ordering::Relaxed),
            self.changes_errors.load(Ordering::Relaxed),
            self.adjust_errors.load(Ordering::Relaxed),
        )
    }
}
