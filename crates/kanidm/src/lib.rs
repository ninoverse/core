//! Provisioning kanidm OAuth2 clients and person accounts through the kanidm CLI.
//!
//! Every operation goes through the `kanidm` command rather than kanidm's REST
//! API: the CLI is the stable, documented surface, and it already manages the
//! session token that the API would otherwise require this crate to obtain and
//! refresh itself.
//!
//! The CLI lives in its own container, so commands are issued through an
//! [`exec::CommandRunner`] — pair [`Kanidm`] with an [`exec::DockerExec`]
//! targeting the `kanidm-cli` container.
//!
//! Provisioning is idempotent. Each step checks for the object before creating
//! it, so a second run converges instead of failing.

pub mod error;

use std::collections::BTreeMap;

use configuration::{Person, Service, ServiceProtectionMode, ServicesConfiguration};
use exec::CommandRunner;
use logger::{info, warn};

pub use crate::error::{KanidmError, KanidmResult};

/// The marker the kanidm CLI puts in the credential-reset link it prints.
const RESET_URL_MARKER: &str = "/ui/reset?token=";

/// A person after provisioning, as reported back to the operator.
#[derive(Debug, Clone)]
pub struct ProvisionedPerson {
    pub name: String,
    pub display_name: String,
    pub groups: Vec<String>,
    /// `Some` only when this run created the account. kanidm has no
    /// non-interactive way for an admin to set a password, so a freshly created
    /// person gets a reset link to complete enrolment themselves. Existing
    /// accounts are left alone rather than having live credentials disturbed.
    pub reset_url: Option<String>,
}

pub struct Kanidm<R> {
    runner: R,
    container: String,
    account: String,
    origin: String,
}

impl<R: CommandRunner> Kanidm<R> {
    pub fn new(runner: R, configuration: &ServicesConfiguration) -> Self {
        Self {
            runner,
            container: configuration.kanidm_cli_container.clone(),
            account: configuration.kanidm_admin.clone(),
            origin: configuration.kanidm_origin.clone(),
        }
    }

    fn argv(&self, args: &[&str]) -> Vec<String> {
        let mut argv: Vec<String> = vec!["kanidm".into()];
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        argv.push("--name".into());
        argv.push(self.account.clone());
        argv
    }

    async fn exec_ok(&self, args: &[&str]) -> KanidmResult<String> {
        let output = self.runner.run(&self.argv(args)).await?;
        if !output.success() {
            return Err(KanidmError::KanidmCli {
                command: args.join(" "),
                code: output.code,
                stderr: output.stderr.trim().to_string(),
            });
        }
        Ok(output.stdout)
    }

    async fn exec_probe(&self, args: &[&str]) -> KanidmResult<bool> {
        Ok(self.runner.run(&self.argv(args)).await?.success())
    }

    /// kanidm's `get` subcommands exit 0 even when nothing matches, printing
    /// "No matching entries" instead — existence is decided by the entry's
    /// `name:` line, not the exit code.
    async fn exec_exists(&self, args: &[&str], name: &str) -> KanidmResult<bool> {
        let output = self.runner.run(&self.argv(args)).await?;
        let needle = format!("name: {name}");
        Ok(output.success() && output.stdout.lines().any(|line| line.trim() == needle))
    }

    pub async fn check_session(&self) -> KanidmResult<()> {
        if self.exec_probe(&["system", "oauth2", "list"]).await? {
            Ok(())
        } else {
            Err(KanidmError::NoKanidmSession {
                container: self.container.clone(),
                account: self.account.clone(),
            })
        }
    }

    pub async fn ensure_group(&self, group: &str) -> KanidmResult<()> {
        if self.exec_exists(&["group", "get", group], group).await? {
            info!(["KANIDM"], "group {} exists", group);
        } else {
            self.exec_ok(&["group", "create", group]).await?;
            info!(["KANIDM"], "group {} created", group);
        }
        Ok(())
    }

    /// `group list-members` prints one quoted SPN per line
    /// (`"alice@example.com"`), so membership is compared on the local part.
    /// Unquoted lines are the CLI's "No matching entries" notice, not members.
    async fn group_members(&self, group: &str) -> KanidmResult<Vec<String>> {
        let output = self
            .runner
            .run(&self.argv(&["group", "list-members", group]))
            .await?;
        if !output.success() {
            return Ok(Vec::new());
        }

        Ok(output
            .stdout
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if !line.starts_with('"') {
                    return None;
                }
                let member = line.trim_matches('"');
                let member = member.split('@').next().unwrap_or(member);
                (!member.is_empty()).then(|| member.to_string())
            })
            .collect())
    }

    /// Creates the account if it is missing. Returns whether it was created.
    pub async fn ensure_person(&self, person: &Person) -> KanidmResult<bool> {
        if self
            .exec_exists(&["person", "get", &person.name], &person.name)
            .await?
        {
            info!(["KANIDM"], "person {} exists", person.name);
            return Ok(false);
        }

        self.exec_ok(&["person", "create", &person.name, &person.display_name])
            .await?;
        info!(["KANIDM"], "person {} created", person.name);
        Ok(true)
    }

    /// Re-applied on every run so the config stays authoritative, matching how
    /// [`Self::provision_client`] re-applies scope maps. Attributes edited by
    /// hand outside the config are reverted.
    async fn apply_person_attributes(&self, person: &Person) -> KanidmResult<()> {
        let mut args = vec!["person", "update", &person.name, "-i", &person.display_name];
        if let Some(mail) = &person.mail {
            args.extend(["-m", mail]);
        }
        if let Some(legal_name) = &person.legal_name {
            args.extend(["-l", legal_name]);
        }

        self.exec_ok(&args).await?;
        info!(["KANIDM"], "attributes applied to {}", person.name);
        Ok(())
    }

    pub async fn ensure_membership(&self, group: &str, name: &str) -> KanidmResult<()> {
        self.ensure_group(group).await?;

        if self
            .group_members(group)
            .await?
            .iter()
            .any(|member| member == name)
        {
            info!(["KANIDM"], "{} already a member of {}", name, group);
            return Ok(());
        }

        self.exec_ok(&["group", "add-members", group, name]).await?;
        info!(["KANIDM"], "{} added to {}", name, group);
        Ok(())
    }

    /// Mints a short-lived link the person uses to enrol their own credentials —
    /// the only non-interactive credential path kanidm offers an admin.
    pub async fn credential_reset_url(&self, name: &str) -> KanidmResult<String> {
        let output = self
            .exec_ok(&["person", "credential", "create-reset-token", name])
            .await?;

        // The CLI builds the link from the URI it dialled — inside the compose
        // network that is `https://kanidmd:8443`, which no browser outside it
        // can reach — so keep only the token and re-anchor it on the public
        // origin. The surrounding QR block and prose make the marker the only
        // reliable anchor; fall back to the raw output if it ever disappears.
        Ok(output
            .split_whitespace()
            .find_map(|word| word.split_once(RESET_URL_MARKER))
            .map(|(_, token)| format!("{}{RESET_URL_MARKER}{token}", self.origin))
            .unwrap_or_else(|| output.trim().to_string()))
    }

    pub async fn provision_person(&self, person: &Person) -> KanidmResult<ProvisionedPerson> {
        info!(["KANIDM"], "provisioning person {}", person.name);

        let created = self.ensure_person(person).await?;
        self.apply_person_attributes(person).await?;

        for group in &person.groups {
            self.ensure_membership(group, &person.name).await?;
        }

        let reset_url = if created {
            Some(self.credential_reset_url(&person.name).await?)
        } else {
            None
        };

        Ok(ProvisionedPerson {
            name: person.name.clone(),
            display_name: person.display_name.clone(),
            groups: person.groups.clone(),
            reset_url,
        })
    }

    pub async fn provision_persons(
        &self,
        configuration: &ServicesConfiguration,
    ) -> KanidmResult<Vec<ProvisionedPerson>> {
        let mut provisioned = Vec::new();
        for person in &configuration.persons {
            provisioned.push(self.provision_person(person).await?);
        }
        Ok(provisioned)
    }

    pub async fn provision_client(
        &self,
        service: &Service,
        configuration: &ServicesConfiguration,
    ) -> KanidmResult<()> {
        info!(
            ["KANIDM"],
            "provisioning {} ({})", service.client_id, service.mode
        );

        self.ensure_group(&service.group).await?;

        let landing = service.base_url(configuration);
        if self
            .exec_exists(
                &["system", "oauth2", "get", &service.client_id],
                &service.client_id,
            )
            .await?
        {
            info!(["KANIDM"], "oauth2 client {} exists", service.client_id);
        } else {
            self.exec_ok(&[
                "system",
                "oauth2",
                "create",
                &service.client_id,
                &service.display_name,
                &landing,
            ])
            .await?;
            info!(
                ["KANIDM"],
                "oauth2 client {} created (landing {})", service.client_id, landing
            );
        }

        match service.redirect_url(configuration) {
            Some(url) => {
                self.exec_ok(&[
                    "system",
                    "oauth2",
                    "add-redirect-url",
                    &service.client_id,
                    &url,
                ])
                .await?;
                info!(["KANIDM"], "redirect url {}", url);
            }
            None if service.mode == ServiceProtectionMode::Native => {
                warn!(
                    ["KANIDM"],
                    "{}: no native_redirect_url set — the app cannot complete a login until you add one",
                    service.client_id
                );
            }
            None => {}
        }

        self.exec_ok(&[
            "system",
            "oauth2",
            "update-scope-map",
            &service.client_id,
            &service.group,
            "openid",
            "profile",
            "email",
            "groups",
        ])
        .await?;
        info!(
            ["KANIDM"],
            "scope map {} -> openid profile email groups", service.group
        );

        self.exec_ok(&[
            "system",
            "oauth2",
            "prefer-short-username",
            &service.client_id,
        ])
        .await?;
        info!(["KANIDM"], "prefer-short-username");

        Ok(())
    }

    pub async fn basic_secret(&self, client_id: &str) -> KanidmResult<String> {
        let output = self
            .exec_ok(&["system", "oauth2", "show-basic-secret", client_id])
            .await?;

        let secret: String = output.split_whitespace().collect();
        if secret.is_empty() {
            return Err(KanidmError::Other(format!(
                "empty basic secret returned for '{client_id}'"
            )));
        }
        Ok(secret)
    }

    pub async fn provision_all(
        &self,
        configuration: &ServicesConfiguration,
    ) -> KanidmResult<BTreeMap<String, String>> {
        let mut secrets = BTreeMap::new();
        // Modes that never present a kanidm identity get no OAuth2 client:
        // creating one would leave a client, a group, and a secret file that
        // nothing ever reads.
        for service in configuration.services.iter().filter(|service| {
            !matches!(
                service.mode,
                ServiceProtectionMode::Proxy | ServiceProtectionMode::Unprotected
            )
        }) {
            self.provision_client(service, configuration).await?;
            secrets.insert(
                service.secret_key(),
                self.basic_secret(&service.client_id).await?,
            );
        }
        Ok(secrets)
    }
}
