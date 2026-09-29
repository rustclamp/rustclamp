//! `clamp key:generate`, `clamp env:encrypt` and `clamp env:decrypt`, like
//! Laravel's: `.env.encrypted` can be committed, and the server decrypts it
//! with a key kept outside the repository.

use std::fs;
use std::path::Path;

use rustclamp::crypto::{Crypt, Key};

/// The environment variable holding the key for `env:decrypt`.
pub const KEY_VARIABLE: &str = "CLAMP_ENV_KEY";

/// Options shared by the commands: `--key=`, `--env=` and `--force`.
#[derive(Default)]
pub struct Options {
    key: Option<String>,
    env: Option<String>,
    force: bool,
}

impl Options {
    /// Reads the flags after the command.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        for arg in args {
            if let Some(key) = arg.strip_prefix("--key=") {
                options.key = Some(key.to_owned());
            } else if let Some(env) = arg.strip_prefix("--env=") {
                if !env
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(format!("--env={env}: use a name such as production"));
                }
                options.env = Some(env.to_owned());
            } else if arg == "--force" {
                options.force = true;
            } else {
                return Err(format!("unknown option {arg}"));
            }
        }
        Ok(options)
    }

    /// `.env` or `.env.{env}`.
    fn file(&self) -> String {
        self.env
            .as_ref()
            .map_or(".env".into(), |env| format!(".env.{env}"))
    }
}

/// Writes a new `APP_KEY` into `root/.env`, or prints it when there is no
/// `.env`. An existing key is kept unless `--force`: replacing it makes
/// everything encrypted with it unreadable.
pub fn key_generate(root: &Path, options: &Options) -> Result<String, String> {
    let key = Key::generate();
    let path = root.join(".env");
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(format!(
            "APP_KEY={key}\n(no .env here; add this line to it)"
        ));
    };
    let has_key = text.lines().any(|line| {
        line.strip_prefix("APP_KEY=")
            .is_some_and(|value| !value.trim().is_empty())
    });
    if has_key && !options.force {
        return Err(".env already has an APP_KEY; --force replaces it, and anything encrypted with the old key becomes unreadable".into());
    }
    fs::write(&path, set_key(&text, &key.to_string()))
        .map_err(|error| format!("cannot write .env: {error}"))?;
    Ok("APP_KEY set in .env".into())
}

/// `text` with its `APP_KEY=` line set to `key`, added when missing.
fn set_key(text: &str, key: &str) -> String {
    let mut found = false;
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| {
            if line.starts_with("APP_KEY=") {
                found = true;
                format!("APP_KEY={key}")
            } else {
                line.to_owned()
            }
        })
        .collect();
    if !found {
        lines.insert(0, format!("APP_KEY={key}"));
    }
    lines.join("\n") + "\n"
}

/// Encrypts `.env` (or `.env.{env}`) into the same name plus `.encrypted`,
/// with `--key=` or a new key, which it prints once.
pub fn encrypt(root: &Path, options: &Options) -> Result<String, String> {
    let file = options.file();
    let plain =
        fs::read(root.join(&file)).map_err(|error| format!("cannot read {file}: {error}"))?;
    let (key, new) = match &options.key {
        Some(text) => (Key::parse(text).ok_or("--key is not a base64: key")?, false),
        None => (Key::generate(), true),
    };
    let target = format!("{file}.encrypted");
    write_new(
        &root.join(&target),
        Crypt::new(&key).encrypt(&plain),
        options.force,
    )?;
    let mut message = format!("Encrypted {file} into {target}.");
    if new {
        message.push_str(&format!(
            "\nKey: {key}\nKeep it outside the repository; decrypt with --key or {KEY_VARIABLE}."
        ));
    }
    Ok(message)
}

/// Decrypts `.env.encrypted` (or `.env.{env}.encrypted`) back into `.env`
/// (or `.env.{env}`), with `--key=` or `CLAMP_ENV_KEY`.
pub fn decrypt(root: &Path, options: &Options, variable: Option<String>) -> Result<String, String> {
    let file = options.file();
    let source = format!("{file}.encrypted");
    let text = options
        .key
        .clone()
        .or(variable)
        .ok_or(format!("pass --key= or set {KEY_VARIABLE}"))?;
    let key = Key::parse(&text).ok_or("the key is not a base64: key")?;
    let sealed = fs::read_to_string(root.join(&source))
        .map_err(|error| format!("cannot read {source}: {error}"))?;
    let plain = Crypt::new(&key).decrypt(&sealed).ok_or(format!(
        "cannot decrypt {source}: wrong key, or the file was changed"
    ))?;
    write_new(&root.join(&file), plain, options.force)?;
    Ok(format!("Decrypted {source} into {file}."))
}

fn write_new(path: &Path, contents: impl AsRef<[u8]>, force: bool) -> Result<(), String> {
    if path.exists() && !force {
        return Err(format!("{} exists; --force replaces it", path.display()));
    }
    fs::write(path, contents).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("clamp-env-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn args(list: &[&str]) -> Options {
        Options::parse(&list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn encrypt_then_decrypt_round_trips() {
        let root = folder("round");
        fs::write(
            root.join(".env.production"),
            "APP_ENV=production\nSECRET=x\n",
        )
        .unwrap();
        let message = encrypt(&root, &args(&["--env=production"])).unwrap();
        let key = message
            .lines()
            .find_map(|line| line.strip_prefix("Key: "))
            .unwrap();
        let sealed = fs::read_to_string(root.join(".env.production.encrypted")).unwrap();
        assert!(!sealed.contains("SECRET"));

        fs::remove_file(root.join(".env.production")).unwrap();
        assert!(
            decrypt(&root, &args(&["--env=production"]), None).is_err(),
            "no key"
        );
        let wrong = Key::generate().to_string();
        assert!(decrypt(&root, &args(&["--env=production"]), Some(wrong)).is_err());
        decrypt(&root, &args(&["--env=production"]), Some(key.to_owned())).unwrap();
        assert_eq!(
            fs::read_to_string(root.join(".env.production")).unwrap(),
            "APP_ENV=production\nSECRET=x\n"
        );
        assert!(
            decrypt(&root, &args(&["--env=production"]), Some(key.to_owned())).is_err(),
            "never overwrites without --force"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn key_generate_keeps_an_existing_key() {
        let root = folder("key");
        fs::write(root.join(".env"), "APP_ENV=local\nAPP_KEY=\n").unwrap();
        key_generate(&root, &Options::default()).unwrap();
        let first = fs::read_to_string(root.join(".env")).unwrap();
        assert!(first.starts_with("APP_ENV=local\nAPP_KEY=base64:"));
        assert!(key_generate(&root, &Options::default()).is_err());
        key_generate(&root, &args(&["--force"])).unwrap();
        assert_ne!(fs::read_to_string(root.join(".env")).unwrap(), first);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn set_key_adds_a_missing_line() {
        assert_eq!(set_key("A=1", "k"), "APP_KEY=k\nA=1\n");
        assert!(Options::parse(&["--env=../x".into()]).is_err());
    }
}
