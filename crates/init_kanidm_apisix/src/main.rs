use std::path::Path;

use configuration::ServicesConfiguration;
use logger::info;

use init_kanidm_apisix::{Reach, native_summary, run_all};
use kanidm::ProvisionedPerson;

const CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/config/default");

#[tokio::main]
async fn main() {
    logger::init();

    let configuration = match ServicesConfiguration::build(Path::new(CONFIG)) {
        Ok(configuration) => configuration,
        Err(configuration_error) => {
            fail(&format!(
                "INIT: failed to load configuration: {configuration_error}"
            ));
        }
    };

    match run_all(&configuration, Reach::ViaDocker).await {
        Ok(summary) => {
            info!(["INIT"], "provisioned {} apisix routes", summary.routes);
            print_person_summary(&summary.persons);
            print_native_summary(&configuration);
        }
        Err(run_error) => {
            let mut message = run_error.to_string();
            if run_error.is_recoverable_by_operator() {
                message.push_str("\nfix the above and re-run; nothing else is wrong");
            }
            fail(&message);
        }
    }
}

// TODO: move to logger crate
// Written straight to stderr rather than through `logger`: the logger drains on
// a background thread with no flush, so anything queued immediately before
// `exit` is lost.
fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

/// Only persons created by this run carry a reset url, so a converged re-run
/// prints nothing here.
fn print_person_summary(persons: &[ProvisionedPerson]) {
    let created: Vec<&ProvisionedPerson> = persons
        .iter()
        .filter(|person| person.reset_url.is_some())
        .collect();
    if created.is_empty() {
        return;
    }

    println!("\nnew persons — send each user their enrolment link");
    for person in created {
        println!("\n  {} ({})", person.display_name, person.name);
        println!("    groups:    {}", person.groups.join(", "));
        if let Some(url) = &person.reset_url {
            println!("    reset url: {url}");
        }
        println!("    expires:   1 hour");
    }
    println!();
}

fn print_native_summary(configuration: &ServicesConfiguration) {
    let natives = native_summary(configuration);
    if natives.is_empty() {
        return;
    }

    println!("\nnative OIDC apps — configure these in the app itself");
    for native in natives {
        println!("\n  {} ({})", native.display_name, native.client_id);
        println!("    discovery:     {}", native.discovery_url);
        println!("    client_id:     {}", native.client_id);
        println!(
            "    client_secret: {}/{}",
            configuration.secrets_dir, native.secret_key
        );
        println!("    access group:  {}", native.group);
        if native.redirect_url_missing {
            println!("    WARNING: native_redirect_url is unset — logins will fail");
        }
    }
    println!();
}
