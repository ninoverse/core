use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{KanidmApisixError, Result};

const SESSION_SECRET: &str = "APISIX_SESSION_SECRET";
const ADMIN_KEY: &str = "APISIX_ADMIN_KEY";
const CLIENT_PREFIX: &str = "OIDC_SECRET_";

const SESSION_SECRET_BYTES: usize = 32;

pub struct SecretStore {
    dir: PathBuf,
}

impl SecretStore {
    pub fn new(dir: &str) -> Result<Self> {
        let dir = PathBuf::from(dir);
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        Ok(Self { dir })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn write_secret(path: &Path, value: &str) -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(value.as_bytes())?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    fn read_secret(path: &Path) -> Result<String> {
        let mut contents = String::new();
        File::open(path)?.read_to_string(&mut contents)?;
        Ok(contents.trim().to_string())
    }

    pub fn write_client_secrets(&self, secrets: &BTreeMap<String, String>) -> Result<()> {
        for (key, value) in secrets {
            Self::write_secret(&self.path(key), value)?;
        }
        Ok(())
    }

    pub fn read_client_secrets(&self) -> Result<BTreeMap<String, String>> {
        let mut secrets = BTreeMap::new();

        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();

            if !name.starts_with(CLIENT_PREFIX) || !entry.file_type()?.is_file() {
                continue;
            }

            secrets.insert(name, Self::read_secret(&entry.path())?);
        }

        Ok(secrets)
    }

    pub fn session_secret(&self) -> Result<(String, bool)> {
        let path = self.path(SESSION_SECRET);
        if path.exists() {
            return Ok((Self::read_secret(&path)?, false));
        }

        let secret = random_hex(SESSION_SECRET_BYTES)?;
        Self::write_secret(&path, &secret)?;
        Ok((secret, true))
    }

    pub fn apisix_admin_key(&self) -> Result<String> {
        let path = self.path(ADMIN_KEY);
        if !path.exists() {
            return Err(KanidmApisixError::MissingAdminKey {
                path: path.display().to_string(),
            });
        }

        let key = Self::read_secret(&path)?;
        if key.is_empty() {
            return Err(KanidmApisixError::MissingAdminKey {
                path: path.display().to_string(),
            });
        }

        Ok(key)
    }
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    File::open("/dev/urandom")?.read_exact(&mut buffer)?;

    let mut hex = String::with_capacity(bytes * 2);
    for byte in buffer {
        use std::fmt::Write as _;
        // Writing into a String cannot fail.
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}
