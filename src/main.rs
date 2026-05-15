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
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap},
};
use reqwest::blocking::Client;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

const DEFAULT_ENDPOINT: &str = "https://api.us.ovhcloud.com/1.0";
const DEFAULT_IPMI_TYPE: &str = "kvmipHtml5URL";
const DEFAULT_IPMI_TTL: &str = "15";
const DETAIL_WORKER_POOL_SIZE: usize = 128;
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

  GET /dedicated/server
  GET /dedicated/server/*

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
    state: Option<String>,
    #[serde(default)]
    reverse: Option<String>,
    #[serde(default)]
    datacenter: Option<String>,
    #[serde(default)]
    ip: Option<String>,
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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerIam {
    #[serde(default)]
    display_name: Option<String>,
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
}

impl ServerDetails {
    fn display_name(&self) -> Option<&str> {
        self.iam
            .as_ref()
            .and_then(|iam| iam.display_name.as_deref())
            .filter(|name| !name.is_empty())
            .or_else(|| self.name.as_deref().filter(|name| !name.is_empty()))
    }
}

fn load_servers(client: &OvhClient) -> Result<Vec<ServerRow>> {
    let service_names = client.list_servers()?;
    let mut rows = service_names
        .into_iter()
        .map(|service_name| ServerRow {
            service_name: service_name.clone(),
            details: None,
            last_error: None,
        })
        .collect::<Vec<_>>();

    let detail_jobs = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (index, row.service_name.clone()))
        .collect::<Vec<_>>();
    let detail_results =
        load_server_details_parallel(client.clone(), detail_jobs, DETAIL_WORKER_POOL_SIZE);

    for (index, result) in detail_results {
        if let Some(row) = rows.get_mut(index) {
            match result {
                Ok(details) => row.details = Some(details),
                Err(error) => row.last_error = Some(error),
            }
        }
    }

    Ok(rows)
}

fn load_server_details_parallel(
    client: OvhClient,
    jobs: Vec<(usize, String)>,
    concurrency: usize,
) -> Vec<(usize, Result<ServerDetails, String>)> {
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
                let result = client
                    .server_details(&service_name)
                    .map_err(|error| format!("{error:#}"));
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
                .with_context(|| format!("IPMI access request failed for {service_name}"));
        }
    }

    let access = wait_for_ipmi_access(&client, &service_name, &access_type)?;

    let Some(url) = access.best_url() else {
        return Err(anyhow!("IPMI access returned no URL: {}", access.raw));
    };

    webbrowser::open(url)
        .with_context(|| format!("could not open browser for {service_name}: {url}"))?;

    Ok(format!("Opened IPMI URL for {service_name}"))
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
                        format!("failed to read IPMI access URL for {service_name}")
                    });
                }
                last_error = Some(message);
                thread::sleep(Duration::from_secs(3));
            }
        }
    }

    Err(anyhow!(
        "IPMI access URL was not ready after 60s for {service_name}. Last OVH response: {}",
        last_error.unwrap_or_else(|| "no response".to_string())
    ))
}

fn is_ipmi_in_progress_error(message: &str) -> bool {
    message.contains("409 Conflict")
        && message
            .to_ascii_lowercase()
            .contains("ipmi interface request access is in progress")
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
    filter_mode: bool,
    filter: String,
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
            filter_mode: false,
            filter: String::new(),
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

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),
            KeyCode::Char('/') => {
                self.filter_mode = true;
                self.status = "Filtering by name".to_string();
            }
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('R') => self.restart_selected(),
            KeyCode::Char('i') => self.open_ipmi(),
            KeyCode::Char('j') | KeyCode::Down => self.next(),
            KeyCode::Char('k') | KeyCode::Up => self.previous(),
            KeyCode::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(8),
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(8),
            KeyCode::Home => self.detail_scroll = 0,
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
        let query = self.filter.trim().to_lowercase();
        self.filtered_indices = self
            .servers
            .iter()
            .enumerate()
            .filter_map(|(index, server)| {
                let name = server.display_name().to_lowercase();
                (query.is_empty() || name.contains(&query)).then_some(index)
            })
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
            "Requesting {} IPMI access for {service_name}...",
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
        if self.filtered_indices.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.filtered_indices.len() - 1);
        self.table_state.select(Some(self.selected));
        self.detail_scroll = 0;
        self.pending_restart = None;
    }

    fn previous(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
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
                "/ filter  r refresh  j/k move  tab focus  pgup/pgdn scroll  i open IPMI  R restart  q quit",
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
        let rows = self.filtered_indices.iter().filter_map(|index| {
            let server = self.servers.get(*index)?;
            Row::new(vec![
                Cell::from(server.display_name().to_string()),
                Cell::from(server.field(|details| details.ip.as_ref())),
                Cell::from(server.field(|details| details.datacenter.as_ref())),
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
                Constraint::Percentage(36),
                Constraint::Percentage(28),
                Constraint::Percentage(18),
                Constraint::Percentage(18),
            ],
        )
        .header(
            Row::new(["Name", "IP", "DC", "State"]).style(
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

    fn draw_details(&self, frame: &mut Frame, area: Rect) {
        let title = if self.focus == Focus::Details {
            " Details "
        } else {
            " Details"
        };
        let lines = self
            .selected_server()
            .map(detail_lines)
            .unwrap_or_else(|| vec![Line::raw("No server loaded")]);
        let details = Paragraph::new(lines)
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(details, area);
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
        push_opt(
            &mut lines,
            "iamDisplayName",
            details
                .iam
                .as_ref()
                .and_then(|iam| iam.display_name.as_deref()),
        );
        push_opt(&mut lines, "name", details.name.as_deref());
        push_opt(&mut lines, "ip", details.ip.as_deref());
        push_opt(&mut lines, "reverse", details.reverse.as_deref());
        push_opt(&mut lines, "datacenter", details.datacenter.as_deref());
        push_opt(&mut lines, "rack", details.rack.as_deref());
        push_opt(&mut lines, "state", details.state.as_deref());
        push_opt(&mut lines, "os", details.os.as_deref());
        push_opt(&mut lines, "range", details.commercial_range.as_deref());
        if let Some(link_speed) = details.link_speed {
            lines.push(kv("linkSpeed", &link_speed.to_string()));
        }
        if let Some(pro) = details.professional_use {
            lines.push(kv("professionalUse", &pro.to_string()));
        }
    }

    if let Some(error) = &server.last_error {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "Errors",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
        for line in error.lines() {
            lines.push(Line::raw(line.to_string()));
        }
    }

    lines
}

fn kv(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:>16}  "), Style::default().fg(Color::Blue)),
        Span::raw(value.to_string()),
    ])
}

fn push_opt(lines: &mut Vec<Line<'static>>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        lines.push(kv(key, value));
    }
}
