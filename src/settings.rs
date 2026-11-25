use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub const SIDEBAR_MIN: f32 = 140.0;
pub const SIDEBAR_MAX: f32 = 600.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub split: bool,
    pub full: bool,
    pub light_mode: bool,
    pub code_zoom: f32,
    pub sidebar_width: f32,
    pub last_repo: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            split: false,
            full: false,
            light_mode: false,
            code_zoom: 1.0,
            sidebar_width: 244.0,
            last_repo: None,
        }
    }
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

pub fn config_path() -> Option<PathBuf> {
    let directory = if cfg!(windows) {
        absolute_env("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        absolute_env("HOME").map(|home| home.join("Library/Application Support"))
    } else {
        absolute_env("XDG_CONFIG_HOME")
            .or_else(|| absolute_env("HOME").map(|home| home.join(".config")))
    }?;
    Some(directory.join("differ").join("settings"))
}

impl Settings {
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(data) => Ok(Self::decode(&data)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    fn decode(data: &[u8]) -> Self {
        let mut settings = Self::default();
        // The native path occupies the bytes after the blank line, so paths
        // containing newlines or, on Unix, non-UTF-8 bytes do not need escaping.
        let Some(separator) = data.windows(2).position(|pair| pair == b"\n\n") else {
            return settings;
        };
        let Ok(header) = std::str::from_utf8(&data[..separator]) else {
            return settings;
        };
        let mut lines = header.lines();
        if lines.next() != Some("differ-settings-v1") {
            return settings;
        }
        for (key, value) in lines.filter_map(|line| line.split_once('=')) {
            match key {
                "split" => settings.split = value.parse().unwrap_or(settings.split),
                "full" => settings.full = value.parse().unwrap_or(settings.full),
                "light" => settings.light_mode = value.parse().unwrap_or(settings.light_mode),
                "zoom" => settings.code_zoom = bounded(value, settings.code_zoom, 0.5, 2.0),
                "sidebar_width" => {
                    settings.sidebar_width =
                        bounded(value, settings.sidebar_width, SIDEBAR_MIN, SIDEBAR_MAX)
                }
                _ => {}
            }
        }
        let bytes = &data[separator + 2..];
        #[cfg(unix)]
        let repo = {
            use std::os::unix::ffi::OsStrExt;
            Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
        };
        #[cfg(not(unix))]
        let repo = std::str::from_utf8(bytes).ok().map(PathBuf::from);
        settings.last_repo = repo.filter(|path| path.is_absolute());
        settings
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let result = (|| {
            write!(
                output,
                "differ-settings-v1\nsplit={}\nfull={}\nlight={}\nzoom={}\nsidebar_width={}\n\n",
                self.split, self.full, self.light_mode, self.code_zoom, self.sidebar_width
            )?;
            if let Some(repo) = &self.last_repo {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    output.write_all(repo.as_os_str().as_bytes())?;
                }
                #[cfg(not(unix))]
                output.write_all(repo.to_string_lossy().as_bytes())?;
            }
            output.flush()?;
            drop(output);
            fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

fn bounded(value: &str, default: f32, min: f32, max: f32) -> f32 {
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map_or(default, |value| value.clamp(min, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_replace_the_previous_version() {
        let directory =
            env::temp_dir().join(format!("differ-settings-test-{}", std::process::id()));
        let file = directory.join("settings");
        let mut settings = Settings {
            split: true,
            full: true,
            light_mode: true,
            code_zoom: 1.3,
            sidebar_width: 300.0,
            last_repo: Some(directory.join("project with spaces")),
        };
        settings.save(&file).unwrap();
        assert_eq!(Settings::load(&file).unwrap(), settings);
        settings.last_repo = None;
        settings.save(&file).unwrap();
        assert_eq!(Settings::load(&file).unwrap(), settings);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_values_keep_safe_defaults_or_are_clamped() {
        let settings = Settings::decode(
            b"differ-settings-v1\nsplit=true\nlight=garbage\nzoom=NaN\nsidebar_width=10\n\nrelative",
        );
        assert!(settings.split);
        assert!(!settings.light_mode);
        assert_eq!(settings.code_zoom, 1.0);
        assert_eq!(settings.sidebar_width, SIDEBAR_MIN);
        assert_eq!(settings.last_repo, None);
        assert_eq!(Settings::decode(b"truncated"), Settings::default());
    }
}
