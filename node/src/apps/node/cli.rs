//! Command-line option parsing.
//!
//! Most configuration is done via config files (see [`config`](../config/index.html) for details).

use std::{io, io::Write, path::PathBuf};

use anyhow::bail;
use structopt::StructOpt;

use crate::config;
use casperlabs_node::{logging, reactor, tls};

// Note: The docstring on `Cli` is the help shown when calling the binary with `--help`.
#[derive(Debug, StructOpt)]
/// CasperLabs blockchain node.
pub enum Cli {
    /// Generate a self-signed node certificate.
    GenerateCert {
        /// Output path base of the certificate. The certificate will be stored as
        /// `output.crt.pem`, while the key will be stored as `output.key.pem`.
        output: PathBuf,
    },
    /// Generate a configuration file from defaults and dump it to stdout.
    GenerateConfig {},

    /// Run the validator node.
    ///
    /// Loads the configuration values from the given configuration file or uses defaults if not
    /// given, then runs the reactor.
    Validator {
        #[structopt(short, long, env)]
        /// Path to configuration file.
        config: Option<PathBuf>,
    },
}

impl Cli {
    /// Executes selected CLI command.
    pub async fn run(self) -> anyhow::Result<()> {
        match self {
            Cli::GenerateCert { output } => {
                if output.file_name().is_none() {
                    bail!("not a valid output path");
                }

                let mut cert_path = output.clone();
                cert_path.set_extension("crt.pem");

                let mut key_path = output;
                key_path.set_extension("key.pem");

                let (cert, key) = tls::generate_node_cert()?;

                tls::save_cert(&cert, cert_path)?;
                tls::save_private_key(&key, key_path)?;
            }
            Cli::GenerateConfig {} => {
                let cfg_str = config::to_string(&reactor::validator::Config::default())?;
                io::stdout().write_all(cfg_str.as_bytes())?;
            }
            Cli::Validator { config } => {
                logging::init()?;
                // We load the specified config, if any, otherwise use defaults.
                let cfg = config
                    .map(config::load_from_file)
                    .transpose()?
                    .unwrap_or_default();

                let mut runner = reactor::Runner::<reactor::validator::Reactor>::new(cfg).await?;
                runner.run().await;
            }
        }

        Ok(())
    }
}
