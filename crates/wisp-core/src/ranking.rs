use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) const DEFAULT_PRIORITY: f64 = 50.0;

type RecencyIndex = HashMap<String, HashMap<String, u64>>;

/// Fig-style suggestion ranking state, persisted after a candidate is chosen.
#[derive(Debug)]
pub(crate) struct RankingStore {
    enabled: bool,
    path: Option<PathBuf>,
    recency: Mutex<RecencyIndex>,
}

impl Default for RankingStore {
    fn default() -> Self {
        Self {
            enabled: false,
            path: None,
            recency: Mutex::new(HashMap::new()),
        }
    }
}

impl RankingStore {
    pub(crate) fn load(enabled: bool, path: PathBuf) -> Self {
        let recency = if enabled {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|source| serde_json::from_str(&source).ok())
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        Self {
            enabled,
            path: Some(path),
            recency: Mutex::new(recency),
        }
    }

    pub(crate) fn priority(&self, command: &str, name: &str, base: f64) -> f64 {
        let base = base.clamp(0.0, 100.0);
        if !self.enabled || name == "../" {
            return base;
        }
        let timestamp = self.recency.lock().ok().and_then(|index| {
            index
                .get(command)
                .and_then(|items| items.get(name))
                .copied()
        });
        ranked_priority(base, timestamp)
    }

    pub(crate) fn record(&self, command: &str, name: &str) -> std::io::Result<()> {
        if !self.enabled || command.is_empty() || name == "../" {
            return Ok(());
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let mut index = self
            .recency
            .lock()
            .map_err(|_| std::io::Error::other("ranking store lock is poisoned"))?;
        index
            .entry(command.to_owned())
            .or_default()
            .insert(name.to_owned(), timestamp);
        let Some(path) = &self.path else {
            return Ok(());
        };
        persist(path, &index)
    }
}

fn ranked_priority(base: f64, timestamp: Option<u64>) -> f64 {
    let Some(timestamp) = timestamp else {
        return base;
    };
    let recency = timestamp as f64 / 10_000_000_000_000.0;
    if (50.0..=75.0).contains(&base) {
        75.0 + recency
    } else {
        base + recency
    }
}

fn persist(path: &Path, index: &RecencyIndex) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let document = serde_json::to_vec(index)?;
    std::fs::write(&temporary, document)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recency_respects_figs_priority_bands() {
        let timestamp = Some(1_700_000_000_000);
        for (base, expected) in [(50.0, 75.17), (74.0, 75.17), (49.0, 49.17), (76.0, 76.17)] {
            assert!((ranked_priority(base, timestamp) - expected).abs() < f64::EPSILON);
        }
    }
}
