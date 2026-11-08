//! Request policy, not provider cache state or a promise of a cache hit.
use std::ffi::OsStr;

use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptCache {
    Off,
    #[default]
    Short,
}

impl PromptCache {
    // Read configuration once at startup, never inside an encode/retry/tool loop.
    pub fn from_setting(value: Option<&OsStr>) -> Result<Self> {
        match value {
            None => Ok(Self::Short),
            Some(value) if value == "short" => Ok(Self::Short),
            Some(value) if value == "off" => Ok(Self::Off),
            // Do not echo arbitrary environment contents in an error.
            Some(_) => bail!("ROBBIE_PROMPT_CACHE must be short or off"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_cache_defaults_to_short_and_accepts_an_explicit_off_switch() {
        assert_eq!(PromptCache::default(), PromptCache::Short);
        assert_eq!(PromptCache::from_setting(None).unwrap(), PromptCache::Short);
        assert_eq!(
            PromptCache::from_setting(Some(OsStr::new("short"))).unwrap(),
            PromptCache::Short
        );
        assert_eq!(
            PromptCache::from_setting(Some(OsStr::new("off"))).unwrap(),
            PromptCache::Off
        );
        for value in ["", "long", "1h", "false", "SHORT", "PRIVATE_INVALID_VALUE"] {
            let error = PromptCache::from_setting(Some(OsStr::new(value)))
                .unwrap_err()
                .to_string();
            assert_eq!(error, "ROBBIE_PROMPT_CACHE must be short or off");
            assert!(!error.contains("PRIVATE_INVALID_VALUE"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn prompt_cache_rejects_non_unicode_configuration_without_echoing_it() {
        use std::os::unix::ffi::OsStrExt;
        let value = OsStr::from_bytes(b"PRIVATE_INVALID_VALUE\xff");
        assert_eq!(
            PromptCache::from_setting(Some(value))
                .unwrap_err()
                .to_string(),
            "ROBBIE_PROMPT_CACHE must be short or off"
        );
    }
}
