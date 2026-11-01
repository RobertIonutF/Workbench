use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "workbench-cli",
    version,
    about = "Your models. Your tools. Your workspace."
)]
struct Cli {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Commands>,
}
#[derive(Subcommand)]
enum Commands {
    Serve {
        #[arg(long, default_value_t = 47831)]
        port: u16,
        #[arg(long)]
        ui_dir: Option<PathBuf>,
        #[arg(long)]
        open: bool,
    },
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    Doctor,
    Chat {
        prompt: String,
        #[arg(long)]
        project: Option<PathBuf>,
        #[arg(long)]
        connection: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, conflicts_with = "read_only")]
        trust_native: bool,
        #[arg(long, conflicts_with = "trust_native")]
        read_only: bool,
    },
}
#[derive(Subcommand)]
enum PluginCommand {
    Init {
        path: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "skill")]
        template: String,
        #[arg(long, default_value = "rust")]
        language: String,
        #[arg(long, default_value = "A reusable Workbench extension.")]
        description: String,
        #[arg(long, conflicts_with = "server_url")]
        server_command: Option<String>,
        #[arg(long, conflicts_with = "server_command")]
        server_url: Option<String>,
        #[arg(long = "server-arg", allow_hyphen_values = true)]
        server_args: Vec<String>,
    },
    Check {
        path: PathBuf,
    },
    Build {
        path: PathBuf,
    },
    Dev {
        path: PathBuf,
    },
    Install {
        path: PathBuf,
    },
    List,
    Test {
        id: String,
    },
    Pack {
        id: String,
        #[arg(long)]
        out: PathBuf,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Commands::Serve {
        port: 47831,
        ui_dir: None,
        open: true,
    });
    let app = match workbench_core::App::new(cli.data_dir.clone()).await {
        Ok(app) => app,
        Err(error) => {
            if let Ok(info) = workbench_core::auth::runtime_info(cli.data_dir) {
                return remote(command, &info).await;
            }
            return Err(error);
        }
    };
    match command {
        Commands::Serve { port, ui_dir, open } => {
            let (address, token) = workbench_core::api::start(app.clone(), port, ui_dir).await?;
            let url = format!("http://{address}/#token={token}");
            println!(
                "Workbench is running locally. This URL contains a private session capability:\n{url}"
            );
            if open {
                let _ = webbrowser::open(&url);
            }
            tokio::signal::ctrl_c().await?;
            for cancel in app.runs.lock().values() {
                cancel.cancel();
            }
            tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        }
        Commands::Plugin { command } => {
            let result = match command {
                PluginCommand::Init {
                    path,
                    name,
                    template,
                    language,
                    description,
                    server_command,
                    server_url,
                    server_args,
                } => workbench_core::plugins::create(
                    &json!({"path":std::path::absolute(path)?,"name":name,"template":template,"language":language,"description":description,"serverCommand":server_command,"serverUrl":server_url,"serverArgs":server_args}),
                )?,
                PluginCommand::Check { path } => {
                    workbench_core::plugins::inspect(&path, "validation")?
                }
                PluginCommand::Build { path } => {
                    workbench_core::plugins::build(&app, &path).await?
                }
                PluginCommand::Dev { path } => {
                    let (address, token) = workbench_core::api::start(app, 0, None).await?;
                    return workbench_core::cli_client::watch(
                        &json!({"baseUrl":format!("http://{address}"),"token":token}),
                        &path,
                    )
                    .await;
                }
                PluginCommand::Install { path } => {
                    workbench_core::plugins::install(&app, &path, None)?
                }
                PluginCommand::List => json!(app.store.all("plugin")?),
                PluginCommand::Test { id } => {
                    let plugin = app.store.get("plugin", &id)?;
                    workbench_core::plugins::test(app, &plugin).await?
                }
                PluginCommand::Pack { id, out } => {
                    workbench_core::plugins::pack(&app, &app.store.get("plugin", &id)?, &out)?
                }
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Commands::Doctor => {
            println!(
                "Workbench {}\nData: {}\nUse Settings → Diagnostics in the desktop app for runtime checks.",
                env!("CARGO_PKG_VERSION"),
                app.data_dir.display()
            );
        }
        Commands::Chat {
            prompt,
            project,
            connection,
            model,
            trust_native,
            read_only,
        } => {
            let (address, token) = workbench_core::api::start(app, 0, None).await?;
            workbench_core::cli_client::chat(
                &json!({"baseUrl":format!("http://{address}"),"token":token}),
                &prompt,
                project.as_deref(),
                connection.as_deref(),
                model.as_deref(),
                if read_only {
                    "read_only"
                } else if trust_native {
                    "trusted_native"
                } else {
                    "ask"
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn remote(command: Commands, info: &serde_json::Value) -> Result<()> {
    use reqwest::Method;
    use workbench_core::cli_client::request;
    let result=match command {
        Commands::Serve{open,..}=>{
            request(info,Method::GET,"state",None).await?;
            if open {webbrowser::open(&format!("{}/#token={}",workbench_core::app::text(info,"baseUrl"),workbench_core::app::text(info,"token")))?;}
            println!("Using the running Workbench runtime.");return Ok(());
        }
        Commands::Doctor=>request(info,Method::GET,"doctor",None).await?,
        Commands::Chat{prompt,project,connection,model,trust_native,read_only}=>return workbench_core::cli_client::chat(info,&prompt,project.as_deref(),connection.as_deref(),model.as_deref(),if read_only{"read_only"}else if trust_native{"trusted_native"}else{"ask"}).await,
        Commands::Plugin{command}=>match command {
            PluginCommand::Init{path,name,template,language,description,server_command,server_url,server_args}=>request(info,Method::POST,"plugins/create",Some(json!({"path":std::path::absolute(path)?,"name":name,"template":template,"language":language,"description":description,"serverCommand":server_command,"serverUrl":server_url,"serverArgs":server_args}))).await?,
            PluginCommand::Check{path}=>workbench_core::plugins::inspect(&path,"validation")?,
            PluginCommand::Build{path}=>request(info,Method::POST,"plugins/build",Some(json!({"path":std::path::absolute(path)?}))).await?,
            PluginCommand::Dev{path}=>return workbench_core::cli_client::watch(info,&path).await,
            PluginCommand::Install{path}=>request(info,Method::POST,"plugins/install",Some(json!({"path":std::path::absolute(path)?}))).await?,
            PluginCommand::List=>request(info,Method::GET,"state",None).await?["plugins"].clone(),
            PluginCommand::Test{id}=>request(info,Method::POST,&format!("plugins/{id}/test"),Some(json!({}))).await?,
            PluginCommand::Pack{id,out}=>request(info,Method::POST,&format!("plugins/{id}/pack"),Some(json!({"outputPath":std::path::absolute(out)?}))).await?,
        }
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
