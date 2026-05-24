use std::{
    env, fs,
    io::{self, Stdout},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Table, TableState, Wrap,
    },
};
use reqwest::blocking::Client;
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

const DEFAULT_ENDPOINT: &str = "https://api.us.ovhcloud.com/1.0";
const DEFAULT_IPMI_TYPE: &str = "kvmipHtml5URL";
const DEFAULT_IPMI_TTL: &str = "15";
const DETAIL_WORKER_POOL_SIZE: usize = 128;
const NETWORK_INTERFACE_CONTROLLER_LINK_TYPES: [&str; 7] = [
    "isolated",
    "private",
    "private_lag",
    "provisioning",
    "provisioning_lag",
    "public",
    "public_lag",
];
const CONFIG_TEMPLATE: &str = r#"# frenchy config
# Env vars with the same names override these values.

application_key = ""
application_secret = ""
consumer_key = ""

endpoint = "https://api.us.ovhcloud.com/1.0"
ipmi_type = "kvmipHtml5URL"
ipmi_ttl = "15"
"#;

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Tui => run_tui(),
        Command::PrintCredentialHelp => {
            print_credential_help();
            Ok(())
        }
    }
}

#[derive(Debug)]
struct Args {
    command: Command,
}

#[derive(Debug)]
enum Command {
    Tui,
    PrintCredentialHelp,
}

impl Args {
    fn parse() -> Self {
        let command = match env::args().nth(1).as_deref() {
            Some("credential-help") | Some("auth-help") => Command::PrintCredentialHelp,
            _ => Command::Tui,
        };
        Self { command }
    }
}

fn print_credential_help() {
    println!(
        r#"frenchy needs OVH API credentials in ~/.config/frenchy/config.toml:

  application_key = "..."
  application_secret = "..."
  consumer_key = "..."

Environment variables with the same names override the config file:

  OVH_APPLICATION_KEY
  OVH_APPLICATION_SECRET
  OVH_CONSUMER_KEY

Optional:

  OVH_ENDPOINT=https://api.us.ovhcloud.com/1.0
  OVH_IPMI_TYPE=kvmipHtml5URL
  OVH_IPMI_TTL=15

Optional config values:

  endpoint = "https://api.us.ovhcloud.com/1.0"
  ipmi_type = "kvmipHtml5URL"
  ipmi_ttl = "15"

Create a consumer key from the OVH API console with read access to:

  https://api.us.ovhcloud.com/createToken/index.cgi?GET=%2Fdedicated%2Fserver&GET=%2Fdedicated%2Fserver%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2Fspecifications%2Fhardware&GET=%2Fdedicated%2Fserver%2F%2A%2Fspecifications%2Fnetwork&GET=%2Fdedicated%2Fserver%2F%2A%2FnetworkInterfaceController&GET=%2Fdedicated%2Fserver%2F%2A%2FnetworkInterfaceController%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualNetworkInterface&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualNetworkInterface%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualMac&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualMac%2F%2A&POST=%2Fdedicated%2Fserver%2F%2A%2Ffeatures%2Fipmi%2Faccess&POST=%2Fdedicated%2Fserver%2F%2A%2Freboot

If OVH returns Invalid account/password, make sure you are using the API
region that owns the account. For OVH US sub-users, use:

  https://us.ovhcloud.com/auth/api/createToken?GET=%2Fdedicated%2Fserver&GET=%2Fdedicated%2Fserver%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2Fspecifications%2Fhardware&GET=%2Fdedicated%2Fserver%2F%2A%2Fspecifications%2Fnetwork&GET=%2Fdedicated%2Fserver%2F%2A%2FnetworkInterfaceController&GET=%2Fdedicated%2Fserver%2F%2A%2FnetworkInterfaceController%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualNetworkInterface&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualNetworkInterface%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualMac&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualMac%2F%2A&POST=%2Fdedicated%2Fserver%2F%2A%2Ffeatures%2Fipmi%2Faccess&POST=%2Fdedicated%2Fserver%2F%2A%2Freboot

  GET /dedicated/server
  GET /dedicated/server/*
  GET /dedicated/server/*/specifications/hardware
  GET /dedicated/server/*/specifications/network
  GET /dedicated/server/*/networkInterfaceController
  GET /dedicated/server/*/networkInterfaceController/*
  GET /dedicated/server/*/virtualNetworkInterface
  GET /dedicated/server/*/virtualNetworkInterface/*
  GET /dedicated/server/*/virtualMac
  GET /dedicated/server/*/virtualMac/*

and write access to request IPMI sessions and restart servers:

  POST /dedicated/server/*/features/ipmi/access
  POST /dedicated/server/*/reboot
"#
    );
}

fn run_tui() -> Result<()> {
    let client = OvhClient::from_env()?;
    let mut terminal = setup_terminal()?;
    let result = App::new(client).run(&mut terminal);
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("failed to create terminal")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

#[derive(Clone)]
struct OvhClient {
    endpoint: String,
    app_key: String,
    app_secret: String,
    consumer_key: String,
    ipmi_type: String,
    ipmi_ttl: String,
    http: Client,
}

impl OvhClient {
    fn from_env() -> Result<Self> {
        let config = Config::load_or_create()?;
        let endpoint = config.endpoint.trim_end_matches('/').to_string();
        let app_key = config.application_key;
        let app_secret = config.application_secret;
        let consumer_key = config.consumer_key;
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("frenchy/0.1")
            .build()?;

        Ok(Self {
            endpoint,
            app_key,
            app_secret,
            consumer_key,
            ipmi_type: config.ipmi_type,
            ipmi_ttl: config.ipmi_ttl,
            http,
        })
    }

    fn list_servers(&self) -> Result<Vec<String>> {
        self.get("/dedicated/server")
    }

    fn server_details(&self, service_name: &str) -> Result<ServerDetails> {
        let raw: Value = self.get(&format!("/dedicated/server/{}", enc(service_name)))?;
        Ok(serde_json::from_value(raw)?)
    }

    fn server_hardware(&self, service_name: &str) -> Result<HardwareSpecs> {
        self.get(&format!(
            "/dedicated/server/{}/specifications/hardware",
            enc(service_name)
        ))
    }

    fn server_network(&self, service_name: &str) -> Result<NetworkSpecs> {
        self.get(&format!(
            "/dedicated/server/{}/specifications/network",
            enc(service_name)
        ))
    }

    fn server_network_interface_controller_addresses(
        &self,
        service_name: &str,
        link_type: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut path = format!(
            "/dedicated/server/{}/networkInterfaceController",
            enc(service_name)
        );
        if let Some(link_type) = link_type {
            path.push_str(&format!("?linkType={}", enc(link_type)));
        }
        self.get(&path)
    }

    fn server_network_interface_controller(
        &self,
        service_name: &str,
        mac: &str,
    ) -> Result<NetworkInterfaceController> {
        self.get(&format!(
            "/dedicated/server/{}/networkInterfaceController/{}",
            enc(service_name),
            enc(mac)
        ))
    }

    fn server_virtual_network_interface_ids(&self, service_name: &str) -> Result<Vec<String>> {
        self.get(&format!(
            "/dedicated/server/{}/virtualNetworkInterface",
            enc(service_name)
        ))
    }

    fn server_virtual_network_interface(
        &self,
        service_name: &str,
        uuid: &str,
    ) -> Result<VirtualNetworkInterface> {
        self.get(&format!(
            "/dedicated/server/{}/virtualNetworkInterface/{}",
            enc(service_name),
            enc(uuid)
        ))
    }

    fn server_virtual_mac_addresses(&self, service_name: &str) -> Result<Vec<String>> {
        self.get(&format!(
            "/dedicated/server/{}/virtualMac",
            enc(service_name)
        ))
    }

    fn server_virtual_mac(&self, service_name: &str, mac_address: &str) -> Result<VirtualMac> {
        self.get(&format!(
            "/dedicated/server/{}/virtualMac/{}",
            enc(service_name),
            enc(mac_address)
        ))
    }

    fn reboot_server(&self, service_name: &str) -> Result<Value> {
        self.request(
            "POST",
            &format!("/dedicated/server/{}/reboot", enc(service_name)),
            "",
        )
    }

    fn get_ipmi_access(&self, service_name: &str, access_type: &str) -> Result<IpmiAccessValue> {
        let raw: Value = self.get(&format!(
            "/dedicated/server/{}/features/ipmi/access?type={}",
            enc(service_name),
            enc(access_type)
        ))?;
        Ok(IpmiAccessValue {
            value: raw
                .get("value")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            url: raw
                .get("url")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            raw,
        })
    }

    fn request_ipmi_access(
        &self,
        service_name: &str,
        access_type: &str,
        ttl: &str,
    ) -> Result<Value> {
        self.post(
            &format!(
                "/dedicated/server/{}/features/ipmi/access",
                enc(service_name)
            ),
            &json!({
                "type": access_type,
                "ttl": ttl,
            }),
        )
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.request("GET", path, "")
    }

    fn post<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        let body = serde_json::to_string(body)?;
        self.request("POST", path, &body)
    }

    fn request<T: DeserializeOwned>(&self, method: &str, path: &str, body: &str) -> Result<T> {
        let url = format!("{}{}", self.endpoint, path);
        let timestamp = Utc::now().timestamp().to_string();
        let signature = self.signature(method, &url, body, &timestamp);

        let mut req = self
            .http
            .request(method.parse()?, &url)
            .header("X-Ovh-Application", &self.app_key)
            .header("X-Ovh-Consumer", &self.consumer_key)
            .header("X-Ovh-Timestamp", &timestamp)
            .header("X-Ovh-Signature", signature);

        if !body.is_empty() {
            req = req
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }

        let response = req.send().with_context(|| format!("{} {}", method, path))?;
        let status = response.status();
        let text = response.text().unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "OVH API {} {} failed: HTTP {}: {}",
                method,
                path,
                status,
                text
            ));
        }

        serde_json::from_str(&text).with_context(|| {
            format!(
                "failed to decode OVH API response for {} {}: {}",
                method, path, text
            )
        })
    }

    fn signature(&self, method: &str, url: &str, body: &str, timestamp: &str) -> String {
        let source = format!(
            "{}+{}+{}+{}+{}+{}",
            self.app_secret, self.consumer_key, method, url, body, timestamp
        );
        let mut hasher = Sha1::new();
        hasher.update(source.as_bytes());
        format!("$1${}", hex::encode(hasher.finalize()))
    }
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    #[serde(default)]
    application_key: String,
    #[serde(default)]
    application_secret: String,
    #[serde(default)]
    consumer_key: String,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    ipmi_type: Option<String>,
    #[serde(default)]
    ipmi_ttl: Option<String>,
}

#[derive(Debug, Clone)]
struct Config {
    application_key: String,
    application_secret: String,
    consumer_key: String,
    endpoint: String,
    ipmi_type: String,
    ipmi_ttl: String,
}

impl Config {
    fn load_or_create() -> Result<Self> {
        let path = config_path()?;
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::write(&path, CONFIG_TEMPLATE)
                .with_context(|| format!("failed to write {}", path.display()))?;
            return Err(anyhow!(
                "created {}; fill in your OVH credentials, then run frenchy again",
                path.display()
            ));
        }

        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let file: FileConfig =
            toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;

        let config = Self {
            application_key: setting("OVH_APPLICATION_KEY", file.application_key),
            application_secret: setting("OVH_APPLICATION_SECRET", file.application_secret),
            consumer_key: setting("OVH_CONSUMER_KEY", file.consumer_key),
            endpoint: setting_opt("OVH_ENDPOINT", file.endpoint, DEFAULT_ENDPOINT),
            ipmi_type: normalize_ipmi_type(&setting_opt(
                "OVH_IPMI_TYPE",
                file.ipmi_type,
                DEFAULT_IPMI_TYPE,
            )),
            ipmi_ttl: setting_opt("OVH_IPMI_TTL", file.ipmi_ttl, DEFAULT_IPMI_TTL),
        };

        config.validate(&path)?;
        Ok(config)
    }

    fn validate(&self, path: &Path) -> Result<()> {
        let missing = [
            ("application_key", &self.application_key),
            ("application_secret", &self.application_secret),
            ("consumer_key", &self.consumer_key),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.trim().is_empty().then_some(name))
        .collect::<Vec<_>>();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "missing {} in {}. Run `cargo run -- credential-help` for details.",
                missing.join(", "),
                path.display()
            ))
        }
    }
}

fn config_path() -> Result<PathBuf> {
    let home = env::var_os("HOME").context("HOME is not set; cannot find config directory")?;
    Ok(PathBuf::from(home).join(".config/frenchy/config.toml"))
}

fn setting(env_name: &str, file_value: String) -> String {
    env::var(env_name).unwrap_or(file_value)
}

fn setting_opt(env_name: &str, file_value: Option<String>, default: &str) -> String {
    env::var(env_name)
        .ok()
        .or(file_value)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn normalize_ipmi_type(value: &str) -> String {
    match value {
        "kvmipHtml5" => "kvmipHtml5URL".to_string(),
        other => other.to_string(),
    }
}

fn enc(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerDetails {
    #[serde(default)]
    iam: Option<ServerIam>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    server_id: Option<Value>,
    #[serde(default)]
    boot_id: Option<Value>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    reverse: Option<String>,
    #[serde(default)]
    datacenter: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    availability_zone: Option<String>,
    #[serde(default)]
    ip: Option<String>,
    #[serde(default)]
    ips: Option<Value>,
    #[serde(default)]
    os: Option<String>,
    #[serde(default)]
    professional_use: Option<bool>,
    #[serde(default)]
    rack: Option<String>,
    #[serde(default)]
    commercial_range: Option<String>,
    #[serde(default)]
    link_speed: Option<u64>,
    #[serde(default)]
    monitoring: Option<bool>,
    #[serde(default)]
    power_state: Option<String>,
    #[serde(default)]
    root_device: Option<String>,
    #[serde(default)]
    support_level: Option<String>,
    #[serde(default)]
    vnis: Vec<VirtualNetworkInterface>,
    #[serde(default)]
    enabled_public_vnis: Vec<String>,
    #[serde(default)]
    enabled_vrack_vnis: Vec<String>,
    #[serde(default)]
    enabled_vrack_aggregation_vnis: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerIam {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    urn: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VirtualNetworkInterface {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    server_name: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    vrack: Option<String>,
    #[serde(
        default,
        alias = "ncis",
        alias = "networkInterfaceController",
        deserialize_with = "deserialize_string_vec"
    )]
    nics: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NetworkInterfaceController {
    #[serde(default)]
    link_type: Option<String>,
    #[serde(default)]
    mac: Option<String>,
    #[serde(default)]
    virtual_network_interface: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HardwareSpecs {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    processor_name: Option<String>,
    #[serde(default)]
    processor_architecture: Option<String>,
    #[serde(default)]
    number_of_processors: Option<u64>,
    #[serde(default)]
    cores_per_processor: Option<u64>,
    #[serde(default)]
    threads_per_processor: Option<u64>,
    #[serde(default)]
    memory_size: Option<Quantity>,
    #[serde(default)]
    disk_groups: Vec<DiskGroup>,
    #[serde(default)]
    default_hardware_raid_type: Option<String>,
    #[serde(default)]
    default_hardware_raid_size: Option<Value>,
    #[serde(default)]
    boot_mode: Option<String>,
    #[serde(default)]
    motherboard: Option<String>,
    #[serde(default)]
    form_factor: Option<String>,
    #[serde(default)]
    expansion_cards: Option<Value>,
    #[serde(default)]
    usb_keys: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiskGroup {
    #[serde(default)]
    disk_group_id: Option<u64>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    disk_size: Option<Quantity>,
    #[serde(default)]
    disk_type: Option<String>,
    #[serde(default)]
    number_of_disks: Option<u64>,
    #[serde(default)]
    raid_controller: Option<Value>,
    #[serde(default)]
    default_hardware_raid_type: Option<String>,
    #[serde(default)]
    default_hardware_raid_size: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct Quantity {
    #[serde(default)]
    value: Option<Value>,
    #[serde(default)]
    unit: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NetworkSpecs {
    #[serde(default)]
    bandwidth: Option<BandwidthSpecs>,
    #[serde(default)]
    connection: Option<Quantity>,
    #[serde(default)]
    ola: Option<OlaSpecs>,
    #[serde(default)]
    routing: Option<RoutingSpecs>,
    #[serde(default)]
    switching: Option<SwitchingSpecs>,
    #[serde(default)]
    traffic: Option<TrafficSpecs>,
    #[serde(default)]
    vmac: Option<VmacSpecs>,
    #[serde(default)]
    vrack: Option<VrackSpecs>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BandwidthSpecs {
    #[serde(default)]
    internet_to_ovh: Option<Quantity>,
    #[serde(default)]
    ovh_to_internet: Option<Quantity>,
    #[serde(default)]
    ovh_to_ovh: Option<Quantity>,
    #[serde(default, rename = "type")]
    bandwidth_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OlaSpecs {
    #[serde(default)]
    available: Option<bool>,
    #[serde(default)]
    available_modes: Vec<OlaMode>,
    #[serde(default, rename = "default")]
    is_default: Option<bool>,
    #[serde(default)]
    interfaces: Vec<OlaInterface>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    supported_modes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OlaMode {
    #[serde(default, rename = "default")]
    is_default: Option<bool>,
    #[serde(default)]
    interfaces: Vec<OlaInterface>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OlaInterface {
    #[serde(default)]
    aggregation: Option<bool>,
    #[serde(default)]
    count: Option<u64>,
    #[serde(default, rename = "type")]
    interface_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoutingSpecs {
    #[serde(default)]
    ipv4: Option<RouteSpecs>,
    #[serde(default)]
    ipv6: Option<RouteSpecs>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RouteSpecs {
    #[serde(default)]
    gateway: Option<String>,
    #[serde(default)]
    ip: Option<String>,
    #[serde(default)]
    network: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwitchingSpecs {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrafficSpecs {
    #[serde(default)]
    input_quota_size: Option<Quantity>,
    #[serde(default)]
    input_quota_used: Option<Quantity>,
    #[serde(default)]
    output_quota_size: Option<Quantity>,
    #[serde(default)]
    output_quota_used: Option<Quantity>,
    #[serde(default)]
    is_throttled: Option<bool>,
    #[serde(default)]
    reset_quota_date: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VmacSpecs {
    #[serde(default)]
    supported: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VrackSpecs {
    #[serde(default)]
    bandwidth: Option<Quantity>,
    #[serde(default, rename = "type")]
    vrack_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VirtualMac {
    #[serde(default)]
    mac_address: Option<String>,
    #[serde(default, rename = "type")]
    mac_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct IpmiAccessValue {
    #[serde(skip)]
    raw: Value,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

impl IpmiAccessValue {
    fn best_url(&self) -> Option<&str> {
        self.value
            .as_deref()
            .or(self.url.as_deref())
            .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
    }
}

#[derive(Debug, Clone)]
struct ServerRow {
    service_name: String,
    details: Option<ServerDetails>,
    hardware: Option<HardwareSpecs>,
    network: Option<NetworkSpecs>,
    network_interface_controllers: Vec<NetworkInterfaceController>,
    virtual_network_interfaces: Vec<VirtualNetworkInterface>,
    virtual_macs: Vec<VirtualMac>,
    load_errors: Vec<String>,
    last_error: Option<String>,
}

impl ServerRow {
    fn display_name(&self) -> &str {
        self.details
            .as_ref()
            .and_then(ServerDetails::display_name)
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.service_name)
    }

    fn field(&self, f: impl FnOnce(&ServerDetails) -> Option<&String>) -> String {
        self.details
            .as_ref()
            .and_then(f)
            .cloned()
            .unwrap_or_else(|| "-".to_string())
    }

    fn hardware_summary(&self) -> String {
        let Some(hardware) = &self.hardware else {
            return "-".to_string();
        };
        let parts = [
            hardware.cpu_label(),
            hardware.memory_label(),
            hardware.disk_summary(),
        ];
        join_non_empty(parts.into_iter().flatten(), " / ").unwrap_or_else(|| "-".to_string())
    }

    fn location_summary(&self) -> String {
        self.details
            .as_ref()
            .and_then(ServerDetails::location_summary)
            .unwrap_or_else(|| "-".to_string())
    }

    fn mac_summary(&self) -> Option<String> {
        let mut macs = self
            .details
            .as_ref()
            .into_iter()
            .flat_map(|details| details.vnis.iter())
            .flat_map(|vni| vni.nics.iter().cloned())
            .chain(
                self.virtual_network_interfaces
                    .iter()
                    .flat_map(|vni| vni.nics.iter().cloned()),
            )
            .chain(
                self.network_interface_controllers
                    .iter()
                    .filter_map(|controller| controller.mac.clone()),
            )
            .chain(
                self.virtual_macs
                    .iter()
                    .filter_map(|mac| mac.mac_address.clone()),
            )
            .collect::<Vec<_>>();
        macs.sort();
        macs.dedup();
        join_non_empty(macs, ", ")
    }

    fn interface_summary(&self) -> Option<String> {
        let details_vnis = self
            .details
            .as_ref()
            .into_iter()
            .flat_map(|details| details.vnis.iter());
        join_non_empty(
            details_vnis
                .chain(self.virtual_network_interfaces.iter())
                .filter_map(VirtualNetworkInterface::summary),
            ", ",
        )
    }

    fn virtual_mac_summary(&self) -> Option<String> {
        join_non_empty(
            self.virtual_macs.iter().filter_map(VirtualMac::summary),
            ", ",
        )
    }

    fn matches_filter(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }

        let haystack = self.search_text();
        query.split_whitespace().all(|term| haystack.contains(term))
    }

    fn search_text(&self) -> String {
        let mut values = Vec::new();
        push_search(&mut values, "service", &self.service_name);
        push_search(&mut values, "display", self.display_name());
        push_search_owned(&mut values, "location", Some(self.location_summary()));
        push_search_owned(&mut values, "hardware", Some(self.hardware_summary()));
        push_search_owned(&mut values, "mac", self.mac_summary());
        push_search_owned(&mut values, "interface", self.interface_summary());
        push_search_owned(&mut values, "vmac", self.virtual_mac_summary());

        if let Some(details) = &self.details {
            if let Some(iam) = &details.iam {
                push_search_opt(&mut values, "iam", iam.display_name.as_deref());
                push_search_opt(&mut values, "iamId", iam.id.as_deref());
                push_search_opt(&mut values, "urn", iam.urn.as_deref());
            }
            push_search_opt(&mut values, "name", details.name.as_deref());
            push_search_value(&mut values, "serverId", details.server_id.as_ref());
            push_search_value(&mut values, "bootId", details.boot_id.as_ref());
            push_search_opt(&mut values, "ip", details.ip.as_deref());
            push_search_value(&mut values, "ips", details.ips.as_ref());
            push_search_opt(&mut values, "reverse", details.reverse.as_deref());
            push_search_opt(&mut values, "datacenter", details.datacenter.as_deref());
            push_search_opt(&mut values, "dc", details.datacenter.as_deref());
            push_search_opt(&mut values, "region", details.region.as_deref());
            push_search_opt(&mut values, "zone", details.availability_zone.as_deref());
            push_search_opt(&mut values, "rack", details.rack.as_deref());
            push_search_opt(&mut values, "state", details.state.as_deref());
            push_search_opt(&mut values, "power", details.power_state.as_deref());
            push_search_opt(&mut values, "os", details.os.as_deref());
            push_search_opt(&mut values, "rootDevice", details.root_device.as_deref());
            push_search_opt(&mut values, "support", details.support_level.as_deref());
            push_search_opt(&mut values, "range", details.commercial_range.as_deref());
            for vni in &details.vnis {
                push_search_owned(&mut values, "vni", vni.summary());
                push_search_owned(&mut values, "mac", vni.mac_summary());
            }
        }

        for vni in &self.virtual_network_interfaces {
            push_search_owned(&mut values, "vni", vni.summary());
            push_search_owned(&mut values, "mac", vni.mac_summary());
        }
        for controller in &self.network_interface_controllers {
            push_search_owned(&mut values, "nic", controller.summary());
            push_search_opt(&mut values, "linkType", controller.link_type.as_deref());
            push_search_opt(&mut values, "mac", controller.mac.as_deref());
            push_search_opt(
                &mut values,
                "vni",
                controller.virtual_network_interface.as_deref(),
            );
        }
        for virtual_mac in &self.virtual_macs {
            push_search_owned(&mut values, "vmac", virtual_mac.summary());
            push_search_owned(&mut values, "mac", virtual_mac.mac_address.clone());
        }

        if let Some(hardware) = &self.hardware {
            push_search_opt(&mut values, "description", hardware.description.as_deref());
            push_search_owned(&mut values, "cpu", hardware.cpu_label());
            push_search_owned(&mut values, "memory", hardware.memory_label());
            push_search_owned(&mut values, "disk", hardware.disk_summary());
        }

        if let Some(network) = &self.network {
            push_search_owned(
                &mut values,
                "bandwidth",
                network.bandwidth.as_ref().and_then(BandwidthSpecs::summary),
            );
            if let Some(routing) = &network.routing {
                push_search_owned(
                    &mut values,
                    "ipv4",
                    routing.ipv4.as_ref().and_then(RouteSpecs::summary),
                );
                push_search_owned(
                    &mut values,
                    "ipv6",
                    routing.ipv6.as_ref().and_then(RouteSpecs::summary),
                );
            }
        }

        values.join(" ").to_lowercase()
    }
}

impl ServerDetails {
    fn display_name(&self) -> Option<&str> {
        self.iam
            .as_ref()
            .and_then(|iam| iam.display_name.as_deref())
            .filter(|name| !name.is_empty())
            .or_else(|| self.name.as_deref().filter(|name| !name.is_empty()))
    }

    fn location_summary(&self) -> Option<String> {
        join_non_empty(
            [
                self.datacenter.clone(),
                self.rack.clone(),
                self.availability_zone
                    .clone()
                    .or_else(|| self.region.clone()),
            ]
            .into_iter()
            .flatten(),
            " / ",
        )
    }
}

impl HardwareSpecs {
    fn cpu_label(&self) -> Option<String> {
        let name = self
            .processor_name
            .as_deref()
            .filter(|name| !name.is_empty())?;
        let topology = match (self.total_cores(), self.total_threads()) {
            (Some(cores), Some(threads)) => Some(format!("{cores}c/{threads}t")),
            (Some(cores), None) => Some(format!("{cores}c")),
            (None, Some(threads)) => Some(format!("{threads}t")),
            (None, None) => None,
        };

        Some(match topology {
            Some(topology) => format!("{name} ({topology})"),
            None => name.to_string(),
        })
    }

    fn topology_label(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(processors) = self.number_of_processors {
            parts.push(plural(processors, "socket"));
        }
        if let Some(cores) = self.cores_per_processor {
            parts.push(format!("{cores} cores/socket"));
        }
        if let Some(threads) = self.threads_per_processor {
            parts.push(format!("{threads} threads/socket"));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    fn memory_label(&self) -> Option<String> {
        self.memory_size.as_ref().and_then(format_memory)
    }

    fn disk_summary(&self) -> Option<String> {
        join_non_empty(self.disk_groups.iter().filter_map(DiskGroup::summary), ", ")
    }

    fn total_cores(&self) -> Option<u64> {
        Some(self.number_of_processors? * self.cores_per_processor?)
    }

    fn total_threads(&self) -> Option<u64> {
        Some(self.number_of_processors? * self.threads_per_processor?)
    }
}

impl DiskGroup {
    fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(count) = self.number_of_disks {
            if let Some(size) = self.disk_size.as_ref().and_then(format_quantity) {
                parts.push(format!("{count}x{size}"));
            } else {
                parts.push(plural(count, "disk"));
            }
        } else if let Some(size) = self.disk_size.as_ref().and_then(format_quantity) {
            parts.push(size);
        }
        if let Some(disk_type) = self.disk_type.as_deref().filter(|value| !value.is_empty()) {
            parts.push(disk_type.to_string());
        }
        if let Some(raid) = self
            .default_hardware_raid_type
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            parts.push(raid.to_string());
        }

        if parts.is_empty() {
            self.description
                .as_deref()
                .filter(|description| !description.is_empty())
                .map(ToString::to_string)
        } else {
            Some(parts.join(" "))
        }
    }

    fn detail(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(description) = self
            .description
            .as_deref()
            .filter(|description| !description.is_empty())
        {
            parts.push(description.to_string());
        } else if let Some(summary) = self.summary() {
            parts.push(summary);
        }
        if let Some(controller) = self.raid_controller.as_ref().and_then(format_value) {
            parts.push(format!("controller {controller}"));
        }
        if let Some(raid) = self
            .default_hardware_raid_type
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            parts.push(format!("default RAID {raid}"));
        }
        if let Some(raid_size) = self
            .default_hardware_raid_size
            .as_ref()
            .and_then(format_quantity_value)
        {
            parts.push(format!("RAID size {raid_size}"));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

impl BandwidthSpecs {
    fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(value) = self.ovh_to_internet.as_ref().and_then(format_quantity) {
            parts.push(format!("out {value}"));
        }
        if let Some(value) = self.internet_to_ovh.as_ref().and_then(format_quantity) {
            parts.push(format!("in {value}"));
        }
        if let Some(value) = self.ovh_to_ovh.as_ref().and_then(format_quantity) {
            parts.push(format!("OVH {value}"));
        }
        if let Some(kind) = self
            .bandwidth_type
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            parts.push(kind.to_string());
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

impl OlaSpecs {
    fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(available) = self.available {
            parts.push(if available {
                "available".to_string()
            } else {
                "unavailable".to_string()
            });
        }
        if let Some(name) = self.name.as_deref().filter(|value| !value.is_empty()) {
            parts.push(format!("mode {name}"));
        }
        if let Some(is_default) = self.is_default {
            parts.push(if is_default {
                "default".to_string()
            } else {
                "custom".to_string()
            });
        }
        if let Some(modes) = join_non_empty(
            self.available_modes.iter().filter_map(OlaMode::name_label),
            ", ",
        ) {
            parts.push(format!("available {modes}"));
        }
        if !self.supported_modes.is_empty() {
            parts.push(format!("supported {}", self.supported_modes.join(", ")));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

impl OlaMode {
    fn name_label(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(name) = self.name.as_deref().filter(|value| !value.is_empty()) {
            parts.push(name.to_string());
        }
        if let Some(is_default) = self.is_default {
            parts.push(if is_default { "default" } else { "custom" }.to_string());
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(label) = self.name_label() {
            parts.push(label);
        }
        if let Some(interfaces) = join_non_empty(
            self.interfaces.iter().filter_map(OlaInterface::summary),
            " + ",
        ) {
            parts.push(interfaces);
        }
        (!parts.is_empty()).then(|| parts.join(": "))
    }
}

impl OlaInterface {
    fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(count) = self.count {
            parts.push(plural(count, "interface"));
        }
        if let Some(interface_type) = self
            .interface_type
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            parts.push(interface_type.to_string());
        }
        if let Some(aggregation) = self.aggregation {
            parts.push(if aggregation {
                "aggregated".to_string()
            } else {
                "standalone".to_string()
            });
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

impl RouteSpecs {
    fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(ip) = self.ip.as_deref().filter(|value| !value.is_empty()) {
            parts.push(format!("ip {ip}"));
        }
        if let Some(network) = self.network.as_deref().filter(|value| !value.is_empty()) {
            parts.push(format!("network {network}"));
        }
        if let Some(gateway) = self.gateway.as_deref().filter(|value| !value.is_empty()) {
            parts.push(format!("gw {gateway}"));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

impl TrafficSpecs {
    fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(value) = self.input_quota_used.as_ref().and_then(format_quantity) {
            parts.push(format!("in used {value}"));
        }
        if let Some(value) = self.input_quota_size.as_ref().and_then(format_quantity) {
            parts.push(format!("in quota {value}"));
        }
        if let Some(value) = self.output_quota_used.as_ref().and_then(format_quantity) {
            parts.push(format!("out used {value}"));
        }
        if let Some(value) = self.output_quota_size.as_ref().and_then(format_quantity) {
            parts.push(format!("out quota {value}"));
        }
        if let Some(throttled) = self.is_throttled {
            parts.push(format!("throttled {throttled}"));
        }
        if let Some(reset) = self
            .reset_quota_date
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            parts.push(format!("resets {reset}"));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

impl NetworkInterfaceController {
    fn summary(&self) -> Option<String> {
        self.summary_with_vni_label(None)
    }

    fn summary_with_vni_label(&self, vni_label: Option<String>) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(mac) = self.mac.as_deref().filter(|value| !value.is_empty()) {
            parts.push(mac.to_string());
        }
        if let Some(link_type) = self.link_type.as_deref().filter(|value| !value.is_empty()) {
            parts.push(link_type.to_string());
        }
        if let Some(vni_label) = vni_label {
            parts.push(vni_label);
        } else if let Some(uuid) = self
            .virtual_network_interface
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            parts.push(format!("VNI {uuid}"));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

impl VirtualNetworkInterface {
    fn label(&self) -> Option<String> {
        self.name
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| self.mode.as_deref().filter(|value| !value.is_empty()))
            .or_else(|| self.uuid.as_deref().filter(|value| !value.is_empty()))
            .map(|value| value.to_string())
    }

    fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(name) = self.name.as_deref().filter(|value| !value.is_empty()) {
            parts.push(name.to_string());
        } else if let Some(uuid) = self.uuid.as_deref().filter(|value| !value.is_empty()) {
            parts.push(uuid.to_string());
        }
        if let Some(server_name) = self
            .server_name
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            parts.push(format!("server {server_name}"));
        }
        if let Some(mode) = self.mode.as_deref().filter(|value| !value.is_empty()) {
            parts.push(mode.to_string());
        }
        if let Some(enabled) = self.enabled {
            parts.push(if enabled { "enabled" } else { "disabled" }.to_string());
        }
        if let Some(vrack) = self.vrack.as_deref().filter(|value| !value.is_empty()) {
            parts.push(format!("vRack {vrack}"));
        }
        if !self.nics.is_empty() {
            parts.push(plural(self.nics.len() as u64, "MAC"));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    fn mac_summary(&self) -> Option<String> {
        join_non_empty(self.nics.iter().cloned(), ", ")
    }
}

impl VirtualMac {
    fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(mac_address) = self
            .mac_address
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            parts.push(mac_address.to_string());
        }
        if let Some(mac_type) = self.mac_type.as_deref().filter(|value| !value.is_empty()) {
            parts.push(mac_type.to_string());
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

fn load_servers(client: &OvhClient) -> Result<Vec<ServerRow>> {
    let service_names = client.list_servers()?;
    let mut rows = service_names
        .into_iter()
        .map(|service_name| ServerRow {
            service_name: service_name.clone(),
            details: None,
            hardware: None,
            network: None,
            network_interface_controllers: Vec::new(),
            virtual_network_interfaces: Vec::new(),
            virtual_macs: Vec::new(),
            load_errors: Vec::new(),
            last_error: None,
        })
        .collect::<Vec<_>>();

    let detail_jobs = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (index, row.service_name.clone()))
        .collect::<Vec<_>>();
    let detail_results =
        load_server_specs_parallel(client.clone(), detail_jobs, DETAIL_WORKER_POOL_SIZE);

    for (index, specs) in detail_results {
        if let Some(row) = rows.get_mut(index) {
            row.details = specs.details;
            row.hardware = specs.hardware;
            row.network = specs.network;
            row.network_interface_controllers = specs.network_interface_controllers;
            row.virtual_network_interfaces = specs.virtual_network_interfaces;
            row.virtual_macs = specs.virtual_macs;
            row.load_errors = specs.errors;
        }
    }

    Ok(rows)
}

#[derive(Debug, Clone, Default)]
struct ServerSpecsLoad {
    details: Option<ServerDetails>,
    hardware: Option<HardwareSpecs>,
    network: Option<NetworkSpecs>,
    network_interface_controllers: Vec<NetworkInterfaceController>,
    virtual_network_interfaces: Vec<VirtualNetworkInterface>,
    virtual_macs: Vec<VirtualMac>,
    errors: Vec<String>,
}

fn load_server_specs(client: &OvhClient, service_name: &str) -> ServerSpecsLoad {
    let mut specs = ServerSpecsLoad::default();

    match client.server_details(service_name) {
        Ok(details) => specs.details = Some(details),
        Err(error) => specs.errors.push(format!("details: {error:#}")),
    }

    match client.server_hardware(service_name) {
        Ok(hardware) => specs.hardware = Some(hardware),
        Err(error) => specs.errors.push(format!("hardware: {error:#}")),
    }

    match client.server_network(service_name) {
        Ok(network) => specs.network = Some(network),
        Err(error) => specs.errors.push(format!("network: {error:#}")),
    }

    load_virtual_network_interfaces(client, service_name, &mut specs);
    load_network_interface_controllers(client, service_name, &mut specs);
    load_virtual_macs(client, service_name, &mut specs);

    specs
}

fn load_virtual_network_interfaces(
    client: &OvhClient,
    service_name: &str,
    specs: &mut ServerSpecsLoad,
) {
    let ids = match client.server_virtual_network_interface_ids(service_name) {
        Ok(ids) => ids,
        Err(error) if is_not_found_error(&format!("{error:#}")) => return,
        Err(error) => {
            specs
                .errors
                .push(format!("virtualNetworkInterface: {error:#}"));
            return;
        }
    };

    for uuid in ids {
        match client.server_virtual_network_interface(service_name, &uuid) {
            Ok(vni) => specs.virtual_network_interfaces.push(vni),
            Err(error) if is_not_found_error(&format!("{error:#}")) => {}
            Err(error) => specs
                .errors
                .push(format!("virtualNetworkInterface/{uuid}: {error:#}")),
        }
    }
}

fn load_network_interface_controllers(
    client: &OvhClient,
    service_name: &str,
    specs: &mut ServerSpecsLoad,
) {
    let mut mac_addresses = Vec::new();
    let mut saw_success = false;
    let mut list_errors = Vec::new();

    match client.server_network_interface_controller_addresses(service_name, None) {
        Ok(addresses) => {
            saw_success = true;
            mac_addresses.extend(addresses);
        }
        Err(error) if is_not_found_error(&format!("{error:#}")) => {}
        Err(error) => list_errors.push(format!("networkInterfaceController: {error:#}")),
    }

    for link_type in NETWORK_INTERFACE_CONTROLLER_LINK_TYPES {
        match client.server_network_interface_controller_addresses(service_name, Some(link_type)) {
            Ok(addresses) => {
                saw_success = true;
                mac_addresses.extend(addresses);
            }
            Err(error) if is_not_found_error(&format!("{error:#}")) => {}
            Err(error) => list_errors.push(format!(
                "networkInterfaceController?linkType={link_type}: {error:#}"
            )),
        }
    }

    mac_addresses.sort();
    mac_addresses.dedup();

    if !saw_success {
        specs.errors.extend(list_errors);
        return;
    }

    for mac in mac_addresses {
        match client.server_network_interface_controller(service_name, &mac) {
            Ok(mut controller) => {
                if controller.mac.is_none() {
                    controller.mac = Some(mac);
                }
                specs.network_interface_controllers.push(controller);
            }
            Err(error) if is_not_found_error(&format!("{error:#}")) => {}
            Err(error) => specs
                .errors
                .push(format!("networkInterfaceController/{mac}: {error:#}")),
        }
    }

    specs.network_interface_controllers.sort_by(|left, right| {
        left.link_type
            .cmp(&right.link_type)
            .then_with(|| left.mac.cmp(&right.mac))
    });
}

fn load_virtual_macs(client: &OvhClient, service_name: &str, specs: &mut ServerSpecsLoad) {
    let mac_addresses = match client.server_virtual_mac_addresses(service_name) {
        Ok(mac_addresses) => mac_addresses,
        Err(error) if is_not_found_error(&format!("{error:#}")) => return,
        Err(error) => {
            specs.errors.push(format!("virtualMac: {error:#}"));
            return;
        }
    };

    for mac_address in mac_addresses {
        match client.server_virtual_mac(service_name, &mac_address) {
            Ok(mut virtual_mac) => {
                if virtual_mac.mac_address.is_none() {
                    virtual_mac.mac_address = Some(mac_address);
                }
                specs.virtual_macs.push(virtual_mac);
            }
            Err(error) if is_not_found_error(&format!("{error:#}")) => {}
            Err(error) => specs
                .errors
                .push(format!("virtualMac/{mac_address}: {error:#}")),
        }
    }
}

fn load_server_specs_parallel(
    client: OvhClient,
    jobs: Vec<(usize, String)>,
    concurrency: usize,
) -> Vec<(usize, ServerSpecsLoad)> {
    if jobs.is_empty() {
        return Vec::new();
    }

    let concurrency = concurrency.max(1);
    let (job_tx, job_rx) = mpsc::channel::<(usize, String)>();
    let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
    let (result_tx, result_rx) = mpsc::channel();
    let worker_count = jobs.len().min(concurrency);

    for _ in 0..worker_count {
        let client = client.clone();
        let job_rx = std::sync::Arc::clone(&job_rx);
        let result_tx = result_tx.clone();
        thread::spawn(move || {
            loop {
                let job = {
                    let Ok(rx) = job_rx.lock() else {
                        break;
                    };
                    rx.recv()
                };
                let Ok((index, service_name)) = job else {
                    break;
                };
                let result = load_server_specs(&client, &service_name);
                let _ = result_tx.send((index, result));
            }
        });
    }
    drop(result_tx);

    let expected = jobs.len();
    for job in jobs {
        if job_tx.send(job).is_err() {
            break;
        }
    }
    drop(job_tx);

    let mut results = Vec::with_capacity(expected);
    for _ in 0..expected {
        match result_rx.recv() {
            Ok(result) => results.push(result),
            Err(_) => break,
        }
    }
    results
}

fn open_ipmi_url(
    client: OvhClient,
    service_name: String,
    access_type: String,
    ttl: String,
) -> Result<String> {
    match client.request_ipmi_access(&service_name, &access_type, &ttl) {
        Ok(_) => {}
        Err(error) if is_ipmi_in_progress_error(&format!("{error:#}")) => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("KVM/IPMI access request failed for {service_name}"));
        }
    }

    let access = wait_for_ipmi_access(&client, &service_name, &access_type)?;

    let Some(url) = access.best_url() else {
        return Err(anyhow!("KVM/IPMI access returned no URL: {}", access.raw));
    };

    webbrowser::open(url)
        .with_context(|| format!("could not open browser for {service_name}: {url}"))?;

    Ok(format!("Opened KVM console for {service_name}"))
}

fn wait_for_ipmi_access(
    client: &OvhClient,
    service_name: &str,
    access_type: &str,
) -> Result<IpmiAccessValue> {
    let mut last_error = None;
    for _ in 0..20 {
        match client.get_ipmi_access(service_name, access_type) {
            Ok(access) => return Ok(access),
            Err(error) => {
                let message = format!("{error:#}");
                if !is_ipmi_in_progress_error(&message) {
                    return Err(error).with_context(|| {
                        format!("failed to read KVM/IPMI access URL for {service_name}")
                    });
                }
                last_error = Some(message);
                thread::sleep(Duration::from_secs(3));
            }
        }
    }

    Err(anyhow!(
        "KVM/IPMI access URL was not ready after 60s for {service_name}. Last OVH response: {}",
        last_error.unwrap_or_else(|| "no response".to_string())
    ))
}

fn is_ipmi_in_progress_error(message: &str) -> bool {
    message.contains("409 Conflict")
        && message
            .to_ascii_lowercase()
            .contains("ipmi interface request access is in progress")
}

fn is_not_found_error(message: &str) -> bool {
    message.contains("HTTP 404") || message.contains("404 Not Found") || message.contains("404 ")
}

fn restart_server(client: OvhClient, service_name: String) -> Result<String> {
    let task = client
        .reboot_server(&service_name)
        .with_context(|| format!("failed to restart {service_name}"))?;
    Ok(format!("Restart requested for {service_name}: {task}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Servers,
    Details,
}

struct App {
    client: OvhClient,
    tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
    servers: Vec<ServerRow>,
    filtered_indices: Vec<usize>,
    selected: usize,
    table_state: TableState,
    detail_scroll: u16,
    focus: Focus,
    status: String,
    loading: bool,
    last_refresh: Option<Instant>,
    ipmi_type: String,
    ipmi_ttl: String,
    pending_restart: Option<String>,
    pending_g: bool,
    filter_mode: bool,
    filter: String,
    visible_server_rows: usize,
    visible_detail_rows: usize,
    visible_detail_width: usize,
}

enum AppEvent {
    ServersLoaded(Result<Vec<ServerRow>, String>),
    IpmiOpened {
        service_name: String,
        result: Result<String, String>,
    },
    Restarted {
        service_name: String,
        result: Result<String, String>,
    },
}

impl App {
    fn new(client: OvhClient) -> Self {
        let ipmi_type = client.ipmi_type.clone();
        let ipmi_ttl = client.ipmi_ttl.clone();
        let (tx, rx) = mpsc::channel();
        Self {
            client,
            tx,
            rx,
            servers: Vec::new(),
            filtered_indices: Vec::new(),
            selected: 0,
            table_state: TableState::default(),
            detail_scroll: 0,
            focus: Focus::Servers,
            status: "Press r to load servers".to_string(),
            loading: false,
            last_refresh: None,
            ipmi_type,
            ipmi_ttl,
            pending_restart: None,
            pending_g: false,
            filter_mode: false,
            filter: String::new(),
            visible_server_rows: 8,
            visible_detail_rows: 8,
            visible_detail_width: 80,
        }
    }

    fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        self.refresh();
        loop {
            self.drain_events();
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(150))?
                && let Event::Key(key) = event::read()?
                && self.handle_key(key)?
            {
                break;
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if self.filter_mode {
            match key.code {
                KeyCode::Esc => {
                    self.filter_mode = false;
                    if self.filter.is_empty() {
                        self.apply_filter();
                    }
                }
                KeyCode::Enter => self.filter_mode = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.apply_filter();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.filter.clear();
                    self.apply_filter();
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.apply_filter();
                }
                _ => {}
            }
            return Ok(false);
        }

        let waiting_for_gg = self.pending_g;
        self.pending_g = false;

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),
            KeyCode::Char('/') => {
                self.filter_mode = true;
                self.status = "Filtering by inventory fields".to_string();
            }
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('R') => self.restart_selected(),
            KeyCode::Char('i') | KeyCode::Char('K') => self.open_ipmi(),
            KeyCode::Char('j') | KeyCode::Down if self.focus == Focus::Details => {
                self.scroll_detail_down_by(1)
            }
            KeyCode::Char('k') | KeyCode::Up if self.focus == Focus::Details => {
                self.scroll_detail_up_by(1)
            }
            KeyCode::Char('j') | KeyCode::Down => self.next(),
            KeyCode::Char('k') | KeyCode::Up => self.previous(),
            KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.focus == Focus::Details =>
            {
                self.scroll_detail_down_by(self.detail_page_size())
            }
            KeyCode::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.focus == Focus::Details =>
            {
                self.scroll_detail_up_by(self.detail_page_size())
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => self.next_page(),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.previous_page()
            }
            KeyCode::Char('g') => {
                if waiting_for_gg {
                    self.first()
                } else {
                    self.pending_g = true;
                    self.status = "Press g again to jump to the first server".to_string();
                }
            }
            KeyCode::Char('G') => self.last(),
            KeyCode::PageDown => self.scroll_detail_down_by(self.detail_page_size()),
            KeyCode::PageUp => self.scroll_detail_up_by(self.detail_page_size()),
            KeyCode::Home => self.detail_scroll = 0,
            KeyCode::End => self.detail_scroll = self.max_detail_scroll(),
            KeyCode::Tab | KeyCode::Enter => {
                self.focus = match self.focus {
                    Focus::Servers => Focus::Details,
                    Focus::Details => Focus::Servers,
                };
            }
            _ => {}
        }
        Ok(false)
    }

    fn refresh(&mut self) {
        if self.loading {
            self.status = "Already loading dedicated servers...".to_string();
            return;
        }
        self.loading = true;
        self.status = "Loading dedicated servers...".to_string();
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = load_servers(&client).map_err(|error| format!("{error:#}"));
            let _ = tx.send(AppEvent::ServersLoaded(result));
        });
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                AppEvent::ServersLoaded(Ok(rows)) => {
                    let count = rows.len();
                    self.servers = rows;
                    self.apply_filter();
                    self.detail_scroll = 0;
                    self.status = format!("Loaded {count} server(s)");
                    self.last_refresh = Some(Instant::now());
                    self.loading = false;
                }
                AppEvent::ServersLoaded(Err(error)) => {
                    self.status = error;
                    self.loading = false;
                }
                AppEvent::IpmiOpened {
                    service_name,
                    result: Ok(message),
                } => {
                    self.clear_server_error(&service_name);
                    self.status = message;
                }
                AppEvent::IpmiOpened {
                    service_name,
                    result: Err(error),
                } => {
                    self.set_server_error(&service_name, error.clone());
                    self.status = error;
                }
                AppEvent::Restarted {
                    service_name,
                    result: Ok(message),
                } => {
                    self.clear_server_error(&service_name);
                    self.pending_restart = None;
                    self.status = message;
                }
                AppEvent::Restarted {
                    service_name,
                    result: Err(error),
                } => {
                    self.set_server_error(&service_name, error.clone());
                    self.pending_restart = None;
                    self.status = error;
                }
            }
        }
    }

    fn apply_filter(&mut self) {
        let query = self.filter.trim();
        self.filtered_indices = self
            .servers
            .iter()
            .enumerate()
            .filter_map(|(index, server)| server.matches_filter(query).then_some(index))
            .collect();
        self.selected = self
            .selected
            .min(self.filtered_indices.len().saturating_sub(1));
        self.table_state
            .select((!self.filtered_indices.is_empty()).then_some(self.selected));
        self.detail_scroll = 0;
        self.pending_restart = None;
    }

    fn selected_server(&self) -> Option<&ServerRow> {
        let index = *self.filtered_indices.get(self.selected)?;
        self.servers.get(index)
    }

    fn set_server_error(&mut self, service_name: &str, error: String) {
        if let Some(server) = self
            .servers
            .iter_mut()
            .find(|server| server.service_name == service_name)
        {
            server.last_error = Some(error);
        }
    }

    fn clear_server_error(&mut self, service_name: &str) {
        if let Some(server) = self
            .servers
            .iter_mut()
            .find(|server| server.service_name == service_name)
        {
            server.last_error = None;
        }
    }

    fn open_ipmi(&mut self) {
        let Some(server) = self.selected_server() else {
            self.status = "No server selected".to_string();
            return;
        };
        let service_name = server.service_name.clone();
        self.status = format!(
            "Requesting {} KVM/IPMI access for {service_name}...",
            self.ipmi_type
        );
        let client = self.client.clone();
        let tx = self.tx.clone();
        let access_type = self.ipmi_type.clone();
        let ttl = self.ipmi_ttl.clone();
        thread::spawn(move || {
            let event_service_name = service_name.clone();
            let result = open_ipmi_url(client, service_name, access_type, ttl)
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(AppEvent::IpmiOpened {
                service_name: event_service_name,
                result,
            });
        });
    }

    fn restart_selected(&mut self) {
        let Some(server) = self.selected_server() else {
            self.status = "No server selected".to_string();
            return;
        };
        let service_name = server.service_name.clone();

        if self.pending_restart.as_deref() != Some(service_name.as_str()) {
            self.pending_restart = Some(service_name.clone());
            self.status = format!("Press R again to confirm restart of {service_name}");
            return;
        }

        self.status = format!("Requesting restart for {service_name}...");
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let event_service_name = service_name.clone();
            let result = restart_server(client, service_name).map_err(|error| format!("{error:#}"));
            let _ = tx.send(AppEvent::Restarted {
                service_name: event_service_name,
                result,
            });
        });
    }

    fn next(&mut self) {
        self.next_by(1);
    }

    fn previous(&mut self) {
        self.previous_by(1);
    }

    fn next_page(&mut self) {
        self.next_by(self.half_page_rows());
    }

    fn previous_page(&mut self) {
        self.previous_by(self.half_page_rows());
    }

    fn first(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.select_server(0);
    }

    fn last(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.select_server(self.filtered_indices.len() - 1);
    }

    fn next_by(&mut self, amount: usize) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.select_server((self.selected + amount).min(self.filtered_indices.len() - 1));
    }

    fn previous_by(&mut self, amount: usize) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.select_server(self.selected.saturating_sub(amount));
    }

    fn half_page_rows(&self) -> usize {
        (self.visible_server_rows / 2).max(1)
    }

    fn detail_page_size(&self) -> usize {
        self.visible_detail_rows.saturating_sub(1).max(1)
    }

    fn scroll_detail_down_by(&mut self, amount: usize) {
        let next = usize::from(self.detail_scroll).saturating_add(amount);
        self.detail_scroll = next.min(usize::from(self.max_detail_scroll())) as u16;
    }

    fn scroll_detail_up_by(&mut self, amount: usize) {
        self.detail_scroll = self.detail_scroll.saturating_sub(amount as u16);
    }

    fn max_detail_scroll(&self) -> u16 {
        let lines = self
            .selected_server()
            .map(detail_lines)
            .unwrap_or_else(|| vec![Line::raw("No server loaded")]);
        let total_rows = wrapped_line_count(&lines, self.visible_detail_width);
        max_scroll(total_rows, self.visible_detail_rows)
    }

    fn select_server(&mut self, selected: usize) {
        self.selected = selected;
        self.table_state.select(Some(self.selected));
        self.detail_scroll = 0;
        self.pending_restart = None;
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(3),
            ])
            .split(area);

        self.draw_header(frame, chunks[0]);
        self.draw_body(frame, chunks[1]);
        self.draw_status(frame, chunks[2]);
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let title = Line::from(vec![
            Span::styled(
                "frenchy",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::raw(
                "/ filter  r refresh  j/k move/scroll  PgUp/PgDn details  ctrl-d/u page  gg/G jump  tab focus  i/K KVM  R restart  q quit",
            ),
        ]);
        frame.render_widget(
            Paragraph::new(title).block(Block::default().borders(Borders::ALL)),
            area,
        );
    }

    fn draw_body(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area);

        self.draw_servers(frame, chunks[0]);
        self.draw_details(frame, chunks[1]);
    }

    fn draw_servers(&mut self, frame: &mut Frame, area: Rect) {
        self.visible_server_rows = usize::from(area.height.saturating_sub(3)).max(1);

        let rows = self.filtered_indices.iter().filter_map(|index| {
            let server = self.servers.get(*index)?;
            Row::new(vec![
                Cell::from(server.display_name().to_string()),
                Cell::from(server.field(|details| details.ip.as_ref())),
                Cell::from(server.location_summary()),
                Cell::from(server.hardware_summary()),
                Cell::from(server.field(|details| details.state.as_ref())),
            ])
            .into()
        });

        let title = if self.filter.is_empty() {
            if self.focus == Focus::Servers {
                format!(" Servers ({}) ", self.servers.len())
            } else {
                format!(" Servers ({})", self.servers.len())
            }
        } else if self.focus == Focus::Servers {
            format!(
                " Servers ({}/{}) filter: {} ",
                self.filtered_indices.len(),
                self.servers.len(),
                self.filter
            )
        } else {
            format!(
                " Servers ({}/{}) filter: {}",
                self.filtered_indices.len(),
                self.servers.len(),
                self.filter
            )
        };
        let table = Table::new(
            rows,
            [
                Constraint::Percentage(28),
                Constraint::Percentage(20),
                Constraint::Percentage(18),
                Constraint::Percentage(24),
                Constraint::Percentage(10),
            ],
        )
        .header(
            Row::new(["Name", "IP", "Location", "Hardware", "State"]).style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .block(Block::default().title(title).borders(Borders::ALL))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(table, area, &mut self.table_state);
    }

    fn draw_details(&mut self, frame: &mut Frame, area: Rect) {
        self.visible_detail_rows = usize::from(area.height.saturating_sub(2)).max(1);
        self.visible_detail_width = usize::from(area.width.saturating_sub(2)).max(1);

        let lines = self
            .selected_server()
            .map(detail_lines)
            .unwrap_or_else(|| vec![Line::raw("No server loaded")]);
        let total_rows = wrapped_line_count(&lines, self.visible_detail_width);
        let max_scroll = max_scroll(total_rows, self.visible_detail_rows);
        self.detail_scroll = self.detail_scroll.min(max_scroll);

        let title = detail_title(
            self.focus == Focus::Details,
            usize::from(self.detail_scroll),
            self.visible_detail_rows,
            total_rows,
        );
        let details = Paragraph::new(lines)
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(details, area);

        if total_rows > self.visible_detail_rows {
            let mut scrollbar_state = ScrollbarState::new(total_rows)
                .position(usize::from(self.detail_scroll))
                .viewport_content_length(self.visible_detail_rows);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("#")
                .track_symbol(Some("|"))
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(Color::Cyan));
            frame.render_stateful_widget(
                scrollbar,
                area.inner(Margin::new(0, 1)),
                &mut scrollbar_state,
            );
        }
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let refresh = self
            .last_refresh
            .map(|instant| format!(" last refresh {}s ago", instant.elapsed().as_secs()))
            .unwrap_or_default();
        let prefix = if self.filter_mode {
            format!("/{}", self.filter)
        } else if self.filter.is_empty() {
            String::new()
        } else {
            format!("filter={}  ", self.filter)
        };
        let status = if self.loading {
            format!("{}{}", self.status, refresh)
        } else {
            format!(
                "{}{}{}  endpoint={}",
                prefix, self.status, refresh, self.client.endpoint
            )
        };
        frame.render_widget(
            Paragraph::new(status)
                .block(Block::default().borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
    }
}

fn detail_lines(server: &ServerRow) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(kv("service", &server.service_name));
    lines.push(kv("display", server.display_name()));

    if let Some(details) = &server.details {
        push_section(&mut lines, "Server");
        push_opt(
            &mut lines,
            "iamDisplayName",
            details
                .iam
                .as_ref()
                .and_then(|iam| iam.display_name.as_deref()),
        );
        push_opt(
            &mut lines,
            "iamId",
            details.iam.as_ref().and_then(|iam| iam.id.as_deref()),
        );
        push_opt(
            &mut lines,
            "iamUrn",
            details.iam.as_ref().and_then(|iam| iam.urn.as_deref()),
        );
        push_opt(&mut lines, "name", details.name.as_deref());
        push_opt_owned(
            &mut lines,
            "serverId",
            details.server_id.as_ref().and_then(format_value),
        );
        push_opt_owned(
            &mut lines,
            "bootId",
            details.boot_id.as_ref().and_then(format_value),
        );
        push_opt(&mut lines, "ip", details.ip.as_deref());
        push_opt_owned(
            &mut lines,
            "ips",
            details.ips.as_ref().and_then(format_value),
        );
        push_opt(&mut lines, "reverse", details.reverse.as_deref());
        push_opt(&mut lines, "datacenter", details.datacenter.as_deref());
        push_opt(&mut lines, "region", details.region.as_deref());
        push_opt(&mut lines, "zone", details.availability_zone.as_deref());
        push_opt(&mut lines, "rack", details.rack.as_deref());
        push_opt(&mut lines, "state", details.state.as_deref());
        push_opt(&mut lines, "power", details.power_state.as_deref());
        push_opt(&mut lines, "os", details.os.as_deref());
        push_opt(&mut lines, "rootDevice", details.root_device.as_deref());
        push_opt(&mut lines, "support", details.support_level.as_deref());
        push_opt(&mut lines, "range", details.commercial_range.as_deref());
        if let Some(link_speed) = details.link_speed {
            lines.push(kv("linkSpeed", &link_speed.to_string()));
        }
        if let Some(monitoring) = details.monitoring {
            lines.push(kv("monitoring", &monitoring.to_string()));
        }
        if let Some(pro) = details.professional_use {
            lines.push(kv("professionalUse", &pro.to_string()));
        }
    }

    if let Some(hardware) = &server.hardware {
        push_hardware_lines(&mut lines, hardware);
    }

    if server.network.is_some()
        || !server.network_interface_controllers.is_empty()
        || !server.virtual_network_interfaces.is_empty()
        || !server.virtual_macs.is_empty()
        || server.details.as_ref().is_some_and(has_network_interfaces)
    {
        push_network_lines(
            &mut lines,
            server.network.as_ref(),
            &server.network_interface_controllers,
            &server.virtual_network_interfaces,
            &server.virtual_macs,
            server.details.as_ref(),
        );
    }

    if !server.load_errors.is_empty() || server.last_error.is_some() {
        push_error_lines(&mut lines, server);
    }

    lines
}

fn push_hardware_lines(lines: &mut Vec<Line<'static>>, hardware: &HardwareSpecs) {
    push_section(lines, "Hardware");
    push_opt(lines, "description", hardware.description.as_deref());
    push_opt_owned(lines, "cpu", hardware.cpu_label());
    push_opt(lines, "cpuArch", hardware.processor_architecture.as_deref());
    push_opt_owned(lines, "topology", hardware.topology_label());
    push_opt_owned(lines, "memory", hardware.memory_label());
    push_opt(lines, "motherboard", hardware.motherboard.as_deref());
    push_opt(lines, "formFactor", hardware.form_factor.as_deref());
    push_opt(lines, "bootMode", hardware.boot_mode.as_deref());

    let default_raid = join_non_empty(
        [
            hardware.default_hardware_raid_type.clone(),
            hardware
                .default_hardware_raid_size
                .as_ref()
                .and_then(format_quantity_value),
        ]
        .into_iter()
        .flatten(),
        ", ",
    );
    push_opt_owned(lines, "defaultRaid", default_raid);

    for (index, disk) in hardware.disk_groups.iter().enumerate() {
        if let Some(detail) = disk.detail() {
            let group = disk
                .disk_group_id
                .map_or_else(|| (index + 1).to_string(), |id| id.to_string());
            lines.push(kv(&format!("diskGroup{group}"), &detail));
        }
    }

    push_opt_owned(
        lines,
        "expansionCards",
        hardware.expansion_cards.as_ref().and_then(format_value),
    );
    push_opt_owned(
        lines,
        "usbKeys",
        hardware.usb_keys.as_ref().and_then(format_value),
    );
}

fn push_network_lines(
    lines: &mut Vec<Line<'static>>,
    network: Option<&NetworkSpecs>,
    network_interface_controllers: &[NetworkInterfaceController],
    virtual_network_interfaces: &[VirtualNetworkInterface],
    virtual_macs: &[VirtualMac],
    details: Option<&ServerDetails>,
) {
    push_section(lines, "Network");

    if let Some(network) = network {
        push_opt_owned(
            lines,
            "connection",
            network.connection.as_ref().and_then(format_quantity),
        );
        push_opt_owned(
            lines,
            "bandwidth",
            network.bandwidth.as_ref().and_then(BandwidthSpecs::summary),
        );
        if let Some(routing) = &network.routing {
            push_opt_owned(
                lines,
                "ipv4Route",
                routing.ipv4.as_ref().and_then(RouteSpecs::summary),
            );
            push_opt_owned(
                lines,
                "ipv6Route",
                routing.ipv6.as_ref().and_then(RouteSpecs::summary),
            );
        }
        if let Some(ola) = &network.ola {
            push_opt_owned(lines, "ola", ola.summary());
            for (index, mode) in ola.available_modes.iter().enumerate() {
                push_opt_owned(lines, &format!("olaMode{}", index + 1), mode.summary());
            }
            for (index, interface) in ola.interfaces.iter().enumerate() {
                push_opt_owned(lines, &format!("olaIf{}", index + 1), interface.summary());
            }
        }
        if let Some(vrack) = &network.vrack {
            let summary = join_non_empty(
                [
                    vrack.bandwidth.as_ref().and_then(format_quantity),
                    vrack.vrack_type.clone(),
                ]
                .into_iter()
                .flatten(),
                ", ",
            );
            push_opt_owned(lines, "vRack", summary);
        }
        if let Some(vmac) = &network.vmac
            && let Some(supported) = vmac.supported
        {
            lines.push(kv("vMAC", &supported.to_string()));
        }
        if let Some(switching) = &network.switching {
            push_opt(lines, "switch", switching.name.as_deref());
        }
        if let Some(traffic) = &network.traffic {
            push_opt_owned(lines, "traffic", traffic.summary());
        }
    }

    for (index, controller) in network_interface_controllers.iter().enumerate() {
        let vni_label = controller
            .virtual_network_interface
            .as_deref()
            .and_then(|uuid| vni_label_for_uuid(uuid, virtual_network_interfaces, details));
        push_opt_owned(
            lines,
            &format!("nic{}", index + 1),
            controller.summary_with_vni_label(vni_label),
        );
    }

    for (index, vni) in virtual_network_interfaces.iter().enumerate() {
        push_opt_owned(lines, &format!("vni{}", index + 1), vni.summary());
        push_opt_owned(lines, &format!("vni{}MACs", index + 1), vni.mac_summary());
    }

    for (index, virtual_mac) in virtual_macs.iter().enumerate() {
        push_opt_owned(
            lines,
            &format!("virtualMac{}", index + 1),
            virtual_mac.summary(),
        );
    }

    if let Some(details) = details {
        push_virtual_network_interfaces(lines, details);
    }
}

fn vni_label_for_uuid(
    uuid: &str,
    virtual_network_interfaces: &[VirtualNetworkInterface],
    details: Option<&ServerDetails>,
) -> Option<String> {
    virtual_network_interfaces
        .iter()
        .chain(details.into_iter().flat_map(|details| details.vnis.iter()))
        .find(|vni| vni.uuid.as_deref() == Some(uuid))
        .and_then(VirtualNetworkInterface::label)
        .map(|label| format!("VNI {label}"))
}

fn push_virtual_network_interfaces(lines: &mut Vec<Line<'static>>, details: &ServerDetails) {
    for (index, vni) in details.vnis.iter().enumerate() {
        push_opt_owned(lines, &format!("vni{}", index + 1), vni.summary());
        push_opt_owned(lines, &format!("vni{}MACs", index + 1), vni.mac_summary());
    }
    if !details.enabled_public_vnis.is_empty() {
        lines.push(kv(
            "publicVNIs",
            &details.enabled_public_vnis.len().to_string(),
        ));
    }
    if !details.enabled_vrack_vnis.is_empty() {
        lines.push(kv(
            "vRackVNIs",
            &details.enabled_vrack_vnis.len().to_string(),
        ));
    }
    if !details.enabled_vrack_aggregation_vnis.is_empty() {
        lines.push(kv(
            "vRackAggVNIs",
            &details.enabled_vrack_aggregation_vnis.len().to_string(),
        ));
    }
}

fn push_error_lines(lines: &mut Vec<Line<'static>>, server: &ServerRow) {
    push_section_with_style(
        lines,
        "Errors",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    );
    for error in &server.load_errors {
        for line in error.lines() {
            lines.push(Line::raw(line.to_string()));
        }
    }
    if let Some(error) = &server.last_error {
        for line in error.lines() {
            lines.push(Line::raw(line.to_string()));
        }
    }
}

fn kv(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:>16}  "), Style::default().fg(Color::Blue)),
        Span::raw(value.to_string()),
    ])
}

fn push_section(lines: &mut Vec<Line<'static>>, title: &str) {
    push_section_with_style(
        lines,
        title,
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
}

fn push_section_with_style(lines: &mut Vec<Line<'static>>, title: &str, style: Style) {
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(title.to_string(), style)));
}

fn push_opt(lines: &mut Vec<Line<'static>>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        lines.push(kv(key, value));
    }
}

fn push_opt_owned(lines: &mut Vec<Line<'static>>, key: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        lines.push(kv(key, &value));
    }
}

fn push_search(values: &mut Vec<String>, key: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() || value == "-" {
        return;
    }
    values.push(value.to_string());
    values.push(format!("{key}:{value}"));
    values.push(format!("{key} {value}"));
}

fn push_search_opt(values: &mut Vec<String>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        push_search(values, key, value);
    }
}

fn push_search_owned(values: &mut Vec<String>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        push_search(values, key, &value);
    }
}

fn push_search_value(values: &mut Vec<String>, key: &str, value: Option<&Value>) {
    push_search_owned(values, key, value.and_then(format_value));
}

fn has_network_interfaces(details: &ServerDetails) -> bool {
    !details.vnis.is_empty()
        || !details.enabled_public_vnis.is_empty()
        || !details.enabled_vrack_vnis.is_empty()
        || !details.enabled_vrack_aggregation_vnis.is_empty()
}

fn detail_title(focused: bool, scroll: usize, viewport_rows: usize, total_rows: usize) -> String {
    let focus_padding = if focused { " " } else { "" };
    if total_rows <= viewport_rows {
        format!(" Details{focus_padding}")
    } else {
        let start = scroll.saturating_add(1).min(total_rows);
        let end = scroll.saturating_add(viewport_rows).min(total_rows);
        format!(" Details {start}-{end}/{total_rows} PgUp/PgDn{focus_padding}")
    }
}

fn wrapped_line_count(lines: &[Line<'_>], width: usize) -> usize {
    let width = width.max(1);
    lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum()
}

fn max_scroll(total_rows: usize, viewport_rows: usize) -> u16 {
    total_rows
        .saturating_sub(viewport_rows)
        .min(usize::from(u16::MAX)) as u16
}

fn format_quantity(quantity: &Quantity) -> Option<String> {
    let value = quantity.value.as_ref().and_then(format_value)?;
    let unit = quantity.unit.as_deref().unwrap_or_default().trim();
    if unit.is_empty() {
        Some(value)
    } else {
        Some(format!("{value} {unit}"))
    }
}

fn format_quantity_value(value: &Value) -> Option<String> {
    serde_json::from_value::<Quantity>(value.clone())
        .ok()
        .and_then(|quantity| format_quantity(&quantity))
        .or_else(|| format_value(value))
}

fn format_memory(quantity: &Quantity) -> Option<String> {
    let unit = quantity.unit.as_deref().unwrap_or_default();
    if unit.eq_ignore_ascii_case("MB")
        && let Some(value) = quantity.value.as_ref().and_then(value_as_f64)
    {
        let gb = value / 1024.0;
        if gb >= 1.0 {
            return Some(format!("{} GB", format_number(gb)));
        }
    }
    format_quantity(quantity)
}

fn format_value(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::String(value) => (!value.is_empty()).then(|| value.to_string()),
        Value::Array(values) => join_non_empty(values.iter().filter_map(format_value), ", "),
        Value::Object(_) => Some(value.to_string()),
    }
}

fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(value) => value.as_f64(),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn format_number(value: f64) -> String {
    if (value - value.round()).abs() < f64::EPSILON {
        format!("{}", value.round() as u64)
    } else {
        let formatted = format!("{value:.1}");
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

fn plural(count: u64, singular: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {singular}s")
    }
}

fn join_non_empty(items: impl IntoIterator<Item = String>, separator: &str) -> Option<String> {
    let items = items
        .into_iter()
        .filter(|item| !item.trim().is_empty())
        .collect::<Vec<_>>();
    (!items.is_empty()).then(|| items.join(separator))
}

fn deserialize_string_vec<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(Vec::new());
    };
    Ok(strings_from_value(value))
}

fn strings_from_value(value: Value) -> Vec<String> {
    match value {
        Value::Null => Vec::new(),
        Value::String(value) => vec![value],
        Value::Array(values) => values
            .into_iter()
            .filter_map(|value| format_value(&value))
            .collect(),
        value => format_value(&value).into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hardware_summary_includes_cpu_memory_and_disks() {
        let hardware: HardwareSpecs = serde_json::from_value(json!({
            "processorName": "Epyc7302",
            "numberOfProcessors": 1,
            "coresPerProcessor": 16,
            "threadsPerProcessor": 32,
            "memorySize": {
                "value": 131072,
                "unit": "MB"
            },
            "diskGroups": [
                {
                    "diskSize": {
                        "value": 1920,
                        "unit": "GB"
                    },
                    "diskType": "NVME",
                    "numberOfDisks": 2
                }
            ]
        }))
        .unwrap();
        let row = ServerRow {
            service_name: "ns123.example.net".to_string(),
            details: None,
            hardware: Some(hardware),
            network: None,
            network_interface_controllers: Vec::new(),
            virtual_network_interfaces: Vec::new(),
            virtual_macs: Vec::new(),
            load_errors: Vec::new(),
            last_error: None,
        };

        assert_eq!(
            row.hardware_summary(),
            "Epyc7302 (16c/32t) / 128 GB / 2x1920 GB NVME"
        );
    }

    #[test]
    fn filters_match_location_ids_and_mac_addresses() {
        let details: ServerDetails = serde_json::from_value(json!({
            "name": "compute-a",
            "serverId": 1033516,
            "datacenter": "bhs1",
            "region": "ca-east-bhs",
            "availabilityZone": "bhs-a",
            "rack": "BHS123A",
            "ip": "40.160.24.210",
            "vnis": [
                {
                    "enabled": true,
                    "mode": "public",
                    "name": "public",
                    "networkInterfaceController": [
                        "aa:bb:cc:dd:ee:01",
                        "aa:bb:cc:dd:ee:02"
                    ],
                    "uuid": "vni-123"
                }
            ]
        }))
        .unwrap();
        let row = ServerRow {
            service_name: "ns1033516.ip-40-160-24.us".to_string(),
            details: Some(details),
            hardware: None,
            network: None,
            network_interface_controllers: vec![
                NetworkInterfaceController {
                    link_type: Some("public_lag".to_string()),
                    mac: Some("aa:bb:cc:dd:ee:01".to_string()),
                    virtual_network_interface: Some("vni-123".to_string()),
                },
                NetworkInterfaceController {
                    link_type: Some("private_lag".to_string()),
                    mac: Some("aa:bb:cc:dd:ee:04".to_string()),
                    virtual_network_interface: Some("vni-456".to_string()),
                },
            ],
            virtual_network_interfaces: vec![VirtualNetworkInterface {
                enabled: Some(true),
                mode: Some("vrack".to_string()),
                name: Some("private".to_string()),
                server_name: Some("compute-a".to_string()),
                uuid: Some("vni-456".to_string()),
                vrack: Some("pn-123".to_string()),
                nics: vec![
                    "aa:bb:cc:dd:ee:03".to_string(),
                    "aa:bb:cc:dd:ee:04".to_string(),
                ],
            }],
            virtual_macs: vec![VirtualMac {
                mac_address: Some("02:00:00:00:00:01".to_string()),
                mac_type: Some("ovh".to_string()),
            }],
            load_errors: Vec::new(),
            last_error: None,
        };

        assert_eq!(row.location_summary(), "bhs1 / BHS123A / bhs-a");
        assert_eq!(
            row.mac_summary(),
            Some(
                "02:00:00:00:00:01, aa:bb:cc:dd:ee:01, aa:bb:cc:dd:ee:02, aa:bb:cc:dd:ee:03, aa:bb:cc:dd:ee:04"
                    .to_string()
            )
        );
        assert!(row.matches_filter("rack:BHS123A"));
        assert!(row.matches_filter("serverId:1033516"));
        assert!(row.matches_filter("zone:bhs-a"));
        assert!(row.matches_filter("aa:bb:cc:dd:ee:04"));
        assert!(row.matches_filter("linkType:private_lag"));
        assert!(row.matches_filter("vmac:02:00:00:00:00:01"));
        assert!(row.matches_filter("bhs1 public"));
    }

    #[test]
    fn network_summaries_include_bandwidth_and_ola_interfaces() {
        let network: NetworkSpecs = serde_json::from_value(json!({
            "bandwidth": {
                "ovhToInternet": {
                    "value": 1,
                    "unit": "Gbps"
                },
                "internetToOvh": {
                    "value": 1,
                    "unit": "Gbps"
                },
                "ovhToOvh": {
                    "value": 10,
                    "unit": "Gbps"
                },
                "type": "included"
            },
            "ola": {
                "available": true,
                "name": "public",
                "availableModes": [
                    {
                        "name": "public(2)+private(2)",
                        "default": true,
                        "interfaces": [
                            {
                                "count": 2,
                                "type": "public",
                                "aggregation": true
                            },
                            {
                                "count": 2,
                                "type": "vrack",
                                "aggregation": true
                            }
                        ]
                    }
                ],
                "supportedModes": ["vrack_aggregation"],
                "default": false,
                "interfaces": [
                    {
                        "count": 2,
                        "type": "public",
                        "aggregation": true
                    }
                ]
            }
        }))
        .unwrap();

        assert_eq!(
            network.bandwidth.as_ref().and_then(BandwidthSpecs::summary),
            Some("out 1 Gbps, in 1 Gbps, OVH 10 Gbps, included".to_string())
        );
        assert_eq!(
            network.ola.as_ref().and_then(OlaSpecs::summary),
            Some(
                "available; mode public; custom; available public(2)+private(2) default; supported vrack_aggregation"
                    .to_string()
            )
        );
        assert_eq!(
            network
                .ola
                .as_ref()
                .and_then(|ola| ola.available_modes[0].summary()),
            Some(
                "public(2)+private(2) default: 2 interfaces public aggregated + 2 interfaces vrack aggregated"
                    .to_string()
            )
        );
        assert_eq!(
            network
                .ola
                .as_ref()
                .and_then(|ola| ola.interfaces[0].summary()),
            Some("2 interfaces public aggregated".to_string())
        );
    }
}
