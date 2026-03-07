use std::sync::Arc;

use clap::{Parser, Subcommand};
use anyhow::Result;
use skyclaw_core::Channel;

#[derive(Parser)]
#[command(name = "skyclaw")]
#[command(about = "Cloud-native Rust AI agent runtime")]
#[command(version)]
struct Cli {
    /// Path to config file
    #[arg(short, long)]
    config: Option<String>,

    /// Runtime mode: cloud, local, or auto
    #[arg(long, default_value = "auto")]
    mode: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the SkyClaw gateway daemon
    Start {
        /// Enable GUI mode (headed browser, desktop interaction)
        #[arg(long)]
        gui: bool,
    },
    /// Interactive CLI chat with the agent
    Chat,
    /// Show gateway status, connected channels, provider health
    Status,
    /// Manage skills
    Skill {
        #[command(subcommand)]
        command: SkillCommands,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Migrate from OpenClaw or ZeroClaw
    Migrate {
        /// Source platform: openclaw or zeroclaw
        #[arg(long)]
        from: String,
        /// Path to source workspace
        path: String,
    },
    /// Show version information
    Version,
}

#[derive(Subcommand)]
enum SkillCommands {
    /// List installed skills
    List,
    /// Show skill details
    Info { name: String },
    /// Install a skill from a path
    Install { path: String },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Validate the current configuration
    Validate,
    /// Show resolved configuration
    Show,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .init();

    // Load configuration
    let config_path = cli.config.as_ref().map(std::path::Path::new);
    let config = skyclaw_core::config::load_config(config_path)?;

    tracing::info!(mode = %cli.mode, "SkyClaw starting");

    match cli.command {
        Commands::Start { gui } => {
            tracing::info!(gui = gui, "Starting SkyClaw gateway");

            // Initialize AI provider
            let provider: Arc<dyn skyclaw_core::Provider> = Arc::from(
                skyclaw_providers::create_provider(&config.provider)?
            );
            tracing::info!(provider = %provider.name(), "Provider initialized");

            // Initialize memory backend
            let memory_url = config.memory.path.clone().unwrap_or_else(|| {
                let data_dir = dirs::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".skyclaw");
                std::fs::create_dir_all(&data_dir).ok();
                format!("sqlite:{}/memory.db?mode=rwc", data_dir.display())
            });
            let memory: Arc<dyn skyclaw_core::Memory> = Arc::from(
                skyclaw_memory::create_memory_backend(&config.memory.backend, &memory_url).await?
            );
            tracing::info!(backend = %config.memory.backend, "Memory initialized");

            // Initialize Telegram channel if configured
            let mut channels: Vec<Arc<dyn skyclaw_core::Channel>> = Vec::new();
            let mut primary_channel: Option<Arc<dyn skyclaw_core::Channel>> = None;
            let mut tg_rx: Option<tokio::sync::mpsc::Receiver<skyclaw_core::types::message::InboundMessage>> = None;

            if let Some(tg_config) = config.channel.get("telegram") {
                if tg_config.enabled {
                    let mut tg = skyclaw_channels::TelegramChannel::new(tg_config)?;
                    tg.start().await?;
                    tg_rx = tg.take_receiver();
                    let tg_arc: Arc<dyn skyclaw_core::Channel> = Arc::new(tg);
                    channels.push(tg_arc.clone());
                    primary_channel = Some(tg_arc.clone());
                    tracing::info!("Telegram channel started");
                }
            }

            // Initialize tools (with channel for file transfer if available)
            let tools = skyclaw_tools::create_tools(&config.tools, primary_channel.clone());
            tracing::info!(count = tools.len(), "Tools initialized");

            // Create agent runtime
            let model = config.provider.model.clone()
                .unwrap_or_else(|| "claude-sonnet-4-6".to_string());
            let agent = Arc::new(skyclaw_agent::AgentRuntime::with_limits(
                provider.clone(),
                memory.clone(),
                tools,
                model.clone(),
                None,
                config.agent.max_turns,
                config.agent.max_context_tokens,
                config.agent.max_tool_rounds,
            ));

            // Unified message channel — all sources (Telegram, heartbeat, future
            // channels) feed into a single processing loop.
            let (msg_tx, mut msg_rx) = tokio::sync::mpsc::channel::<skyclaw_core::types::message::InboundMessage>(32);

            // Wire Telegram messages into the unified channel
            if let Some(mut tg_rx) = tg_rx {
                let tx = msg_tx.clone();
                tokio::spawn(async move {
                    while let Some(msg) = tg_rx.recv().await {
                        if tx.send(msg).await.is_err() { break; }
                    }
                });
            }

            // Start heartbeat if enabled
            let workspace_path = dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".skyclaw")
                .join("workspace");
            std::fs::create_dir_all(&workspace_path).ok();

            if config.heartbeat.enabled {
                let heartbeat_chat_id = config.heartbeat.report_to.clone()
                    .unwrap_or_else(|| "heartbeat".to_string());
                let runner = skyclaw_automation::HeartbeatRunner::new(
                    config.heartbeat.clone(),
                    workspace_path.clone(),
                    heartbeat_chat_id,
                );
                let hb_tx = msg_tx.clone();
                tokio::spawn(async move {
                    runner.run(hb_tx).await;
                });
                tracing::info!(
                    interval = %config.heartbeat.interval,
                    checklist = %config.heartbeat.checklist,
                    "Heartbeat runner started"
                );
            }

            // Unified message processing loop
            if let Some(sender) = primary_channel.clone() {
                let agent_clone = agent.clone();
                let ws_path = workspace_path.clone();
                tokio::spawn(async move {
                    while let Some(mut inbound) = msg_rx.recv().await {
                        let agent = agent_clone.clone();
                        let sender = sender.clone();
                        let workspace_path = ws_path.clone();
                        tokio::spawn(async move {
                            // Download attachments and save to workspace
                            if !inbound.attachments.is_empty() {
                                if let Some(ft) = sender.file_transfer() {
                                    match ft.receive_file(&inbound).await {
                                        Ok(files) => {
                                            let mut file_notes = Vec::new();
                                            for file in &files {
                                                let save_path = workspace_path.join(&file.name);
                                                if let Err(e) = tokio::fs::write(&save_path, &file.data).await {
                                                    tracing::error!(error = %e, file = %file.name, "Failed to save attachment");
                                                } else {
                                                    tracing::info!(file = %file.name, size = file.size, "Saved attachment to workspace");
                                                    file_notes.push(format!(
                                                        "[File received: {} ({}, {} bytes) — saved to workspace/{}]",
                                                        file.name, file.mime_type, file.size, file.name
                                                    ));
                                                }
                                            }
                                            if !file_notes.is_empty() {
                                                let prefix = file_notes.join("\n");
                                                let existing = inbound.text.take().unwrap_or_default();
                                                inbound.text = Some(format!("{}\n{}", prefix, existing));
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!(error = %e, "Failed to download attachments");
                                        }
                                    }
                                }
                            }

                            let mut session = skyclaw_core::types::session::SessionContext {
                                session_id: format!("{}-{}", inbound.channel, inbound.chat_id),
                                user_id: inbound.user_id.clone(),
                                channel: inbound.channel.clone(),
                                chat_id: inbound.chat_id.clone(),
                                history: Vec::new(),
                                workspace_path,
                            };

                            match agent.process_message(&inbound, &mut session).await {
                                Ok(reply) => {
                                    if let Err(e) = sender.send_message(reply).await {
                                        tracing::error!(error = %e, "Failed to send reply");
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "Agent processing error");
                                    let error_reply = skyclaw_core::types::message::OutboundMessage {
                                        chat_id: inbound.chat_id.clone(),
                                        text: format!("Error: {}", e),
                                        reply_to: Some(inbound.id.clone()),
                                        parse_mode: None,
                                    };
                                    let _ = sender.send_message(error_reply).await;
                                }
                            }
                        });
                    }
                });
            }

            // Start the gateway server
            let gate = skyclaw_gateway::SkyGate::new(
                channels,
                agent,
                config.gateway.clone(),
            );

            println!("SkyClaw gateway starting...");
            println!("  Mode: {}", cli.mode);
            println!("  GUI: {}", gui);
            println!("  Gateway: http://{}:{}", config.gateway.host, config.gateway.port);
            println!("  Health: http://{}:{}/health", config.gateway.host, config.gateway.port);

            gate.start().await?;
        }
        Commands::Chat => {
            println!("SkyClaw interactive chat");
            println!("Type 'exit' to quit.");
            // TODO: Start CLI channel directly
        }
        Commands::Status => {
            println!("SkyClaw Status");
            println!("  Mode: {}", config.skyclaw.mode);
            println!("  Gateway: {}:{}", config.gateway.host, config.gateway.port);
            println!("  Provider: {}", config.provider.name.as_deref().unwrap_or("not configured"));
            println!("  Memory: {}", config.memory.backend);
            println!("  Vault: {}", config.vault.backend);
        }
        Commands::Skill { command } => match command {
            SkillCommands::List => {
                println!("Installed skills:");
                // TODO: List skills from registry
            }
            SkillCommands::Info { name } => {
                println!("Skill info: {}", name);
                // TODO: Show skill details
            }
            SkillCommands::Install { path } => {
                println!("Installing skill from: {}", path);
                // TODO: Install skill
            }
        },
        Commands::Config { command } => match command {
            ConfigCommands::Validate => {
                println!("Configuration valid.");
                println!("  Gateway: {}:{}", config.gateway.host, config.gateway.port);
                println!("  Provider: {}", config.provider.name.as_deref().unwrap_or("none"));
                println!("  Memory backend: {}", config.memory.backend);
                println!("  Channels: {}", config.channel.len());
            }
            ConfigCommands::Show => {
                let output = toml::to_string_pretty(&config)?;
                println!("{}", output);
            }
        },
        Commands::Migrate { from, path } => {
            println!("Migrating from {} at {}", from, path);
            // TODO: Run migration
        }
        Commands::Version => {
            println!("skyclaw {}", env!("CARGO_PKG_VERSION"));
            println!("Cloud-native Rust AI agent runtime");
        }
    }

    Ok(())
}
