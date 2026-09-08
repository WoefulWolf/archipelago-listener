use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use archipelago_rs::{
    Connection, ConnectionOptions, ConnectionState, ConnectionStateType, Error::ConnectionRefused,
    Event, Print, RichText, tags,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::{
    task::{JoinHandle, JoinSet},
    time::MissedTickBehavior,
};

const PORT_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MONITOR_TICK_INTERVAL: Duration = Duration::from_secs(1);

struct Config {
    target_host: String,
    start_port: u16,
    end_port: u16,
    scan_interval: Duration,
    bot_name: String,
    slots: Vec<String>,
    webhook_url: String,
}

impl Config {
    fn from_env() -> Self {
        fn var_or(name: &str, default: &str) -> String {
            std::env::var(name).unwrap_or_else(|_| default.to_string())
        }

        Self {
            target_host: var_or("TARGET_HOST", "localhost"),
            start_port: var_or("START_PORT", "50000")
                .parse()
                .expect("invalid START_PORT"),
            end_port: var_or("END_PORT", "50009")
                .parse()
                .expect("invalid END_PORT"),
            scan_interval: Duration::from_millis(
                var_or("SCAN_INTERVAL_MS", "30000")
                    .parse()
                    .expect("invalid SCAN_INTERVAL_MS"),
            ),
            bot_name: var_or("BOT_NAME", "Archie"),
            slots: std::env::var("SLOTS")
                .map(|s| {
                    s.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            webhook_url: var_or("WEBHOOK_URL", ""),
        }
    }
}

#[tokio::main]
async fn main() {
    let config = Config::from_env();

    println!("Starting archipelago-listener...");
    println!(
        "Target range: {}:{} -> {}:{}",
        config.target_host, config.start_port, config.target_host, config.end_port
    );

    if config.webhook_url.is_empty() {
        eprintln!("WARNING: WEBHOOK_URL is not set.");
    }
    if config.slots.is_empty() {
        eprintln!("SLOTS is not set; there is nothing to connect as. Exiting.");
        return;
    }

    scan_loop(config).await;
}

async fn check_remote_container_port(host: &str, port: u16) -> bool {
    let connect = tokio::net::TcpStream::connect((host, port));
    matches!(
        tokio::time::timeout(PORT_PROBE_TIMEOUT, connect).await,
        Ok(Ok(_))
    )
}

async fn scan_loop(config: Config) {
    let webhook = Arc::new(WebhookClient {
        url: config.webhook_url.clone(),
        reqwest: Client::new(),
        bot_name: config.bot_name.clone(),
    });

    let mut monitors: HashMap<u16, JoinHandle<()>> = HashMap::new();

    let mut ticker = tokio::time::interval(config.scan_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;

        monitors.retain(|port, handle| {
            if handle.is_finished() {
                println!("[Port {port}] Monitor stopped; port is eligible again.");
                false
            } else {
                true
            }
        });

        let mut probes = JoinSet::new();
        for port in config.start_port..=config.end_port {
            if monitors.contains_key(&port) {
                continue;
            }
            let host = config.target_host.clone();
            probes.spawn(async move { (port, check_remote_container_port(&host, port).await) });
        }

        let mut open_ports = Vec::new();
        while let Some(result) = probes.join_next().await {
            match result {
                Ok((port, true)) => open_ports.push(port),
                Ok((_, false)) => {}
                Err(err) => eprintln!("Port probe task failed: {err}"),
            }
        }
        open_ports.sort_unstable();

        for port in open_ports {
            println!("[Port {port}] Server detected online in container! Connecting...");
            match RoomMonitor::new(&config.target_host, port, &config.slots).await {
                Ok(monitor) => {
                    println!("[Port {port}] Connected.");
                    monitors.insert(port, monitor.run(port, Arc::clone(&webhook)));
                }
                Err(err) => eprintln!("[Port {port}] Not monitoring: {err}"),
            }
        }
    }
}

struct RoomMonitor {
    connection: Connection<()>,
}

struct WebhookClient {
    url: String,
    reqwest: Client,
    bot_name: String,
}

impl RoomMonitor {
    pub async fn new(host: &str, port: u16, slots: &[String]) -> Result<Self, String> {
        let url = format!("ws://{host}:{port}");

        for slot in slots {
            let mut connection = Connection::new(
                url.clone(),
                slot.as_str(),
                None::<&str>,
                ConnectionOptions::new().tags([tags::TRACKER]),
            );

            let deadline = Instant::now() + HANDSHAKE_TIMEOUT;

            loop {
                connection.update();

                match connection.state_type() {
                    ConnectionStateType::Connected => return Ok(Self { connection }),
                    ConnectionStateType::Disconnected => {
                        let fatal = match connection.state() {
                            ConnectionState::Disconnected(ConnectionRefused(_)) => None,
                            ConnectionState::Disconnected(error) => Some(error.to_string()),
                            _ => None,
                        };

                        match fatal {
                            Some(error) => {
                                eprintln!("[Port {port}] Slot \"{slot}\" failed: {error}")
                            }
                            None => eprintln!("[Port {port}] Slot \"{slot}\" was refused"),
                        }
                        break;
                    }
                    ConnectionStateType::Connecting => {}
                }

                if Instant::now() >= deadline {
                    eprintln!("[Port {port}] Slot \"{slot}\" timed out while connecting");
                    break;
                }

                tokio::time::sleep(HANDSHAKE_POLL_INTERVAL).await;
            }
        }

        Err("no slots were valid".to_string())
    }

    pub fn run(mut self, port: u16, webhook: Arc<WebhookClient>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(MONITOR_TICK_INTERVAL).await;

                for event in self.connection.update() {
                    match event {
                        Event::Print(print) => {
                            if let Err(err) = Self::process_print(&webhook, print).await {
                                eprintln!("[Port {port}] Failed to process print event: {err}")
                            }
                        }
                        Event::Error(err) => {
                            eprintln!("[Port {port}] Connection error: {err}")
                        }
                        _ => {}
                    }
                }

                if self.connection.state_type() == ConnectionStateType::Disconnected {
                    println!("[Port {port}] Server went offline inside container. Cleaning up.");
                    return;
                }
            }
        })
    }

    async fn process_print(client: &WebhookClient, print: Print) -> Result<(), reqwest::Error> {
        match print {
            Print::ItemSend { data, .. } | Print::Hint { data, .. } => {
                Self::process_rich_texts(client, data).await?
            }
            _ => {}
        }
        Ok(())
    }

    async fn process_rich_texts(
        client: &WebhookClient,
        rich_texts: Vec<RichText>,
    ) -> Result<(), reqwest::Error> {
        let mut strings = Vec::new();
        for entry in rich_texts {
            match entry {
                RichText::Player(player) => strings.push(player.alias().to_string()),
                RichText::PlayerName(name) => strings.push(name),
                RichText::Item { item, .. } => strings.push(item.name().to_string()),
                RichText::Location { location, .. } => strings.push(location.name().to_string()),
                RichText::EntranceName(name) => strings.push(name),
                RichText::Color { text, .. } => strings.push(text),
                RichText::Text(text) => strings.push(text),
            }
        }

        let formatted_string = strings.join(" ");

        let body = FluxerMessage {
            username: client.bot_name.clone(),
            embeds: vec![FluxerEmbed {
                title: "".to_string(),
                description: formatted_string,
                color: 0xff0000,
                fields: Vec::new(),
            }],
        };

        let _response = client
            .reqwest
            .post(client.url.clone())
            .json(&body)
            .send()
            .await?;

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct FluxerMessage {
    username: String,
    embeds: Vec<FluxerEmbed>,
}

#[derive(Serialize, Deserialize, Debug)]
struct FluxerEmbed {
    title: String,
    description: String,
    color: u32,
    fields: Vec<String>,
}
