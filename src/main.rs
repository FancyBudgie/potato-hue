use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::time::sleep;

const POTATO_STATE_URL: &str = "https://potpotato.xyz/api/state";

#[derive(Debug)]
struct Config {
    bridge_ip: String,
    app_key: String,
    light_id: String,
    poll_every: Duration,
    pulse_on: Duration,
    pulse_gap: Duration,
    pulse_bri: u8,
    pulse_style: PulseStyle,
    holder: Option<HolderConfig>,
    state_file: PathBuf,
}

#[derive(Debug, Clone, Copy)]
enum PulseStyle {
    Preserve,
    Temperature(u16),
    Xy { x: f64, y: f64 },
}

#[derive(Debug, Clone)]
struct HolderConfig {
    watched_address: String,
    start_bri: u8,
    end_bri: u8,
    start_xy: [f64; 2],
    end_xy: [f64; 2],
    start_ct: u16,
    end_ct: u16,
}

#[derive(Debug, Deserialize)]
struct PotatoResponse {
    state: PotatoState,
}

#[derive(Debug, Deserialize)]
struct PotatoState {
    #[serde(rename = "holderCount")]
    holder_count: u64,
    #[serde(rename = "currentOwner", default)]
    current_owner: Option<String>,
    #[serde(rename = "purchasedAt", default)]
    purchased_at: Option<i64>,
    #[serde(default)]
    deadline: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct HueLight {
    name: String,
    #[serde(default)]
    productname: Option<String>,
    state: HueState,
    #[serde(default)]
    capabilities: HueCapabilities,
}

#[derive(Debug, Default, Deserialize)]
struct HueCapabilities {
    #[serde(default)]
    control: HueControl,
}

#[derive(Debug, Default, Deserialize)]
struct HueControl {
    #[serde(default)]
    colorgamut: Option<Vec<[f64; 2]>>,
    #[serde(default)]
    ct: Option<HueTemperatureRange>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct HueTemperatureRange {
    min: u16,
    max: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct HueState {
    on: bool,
    #[serde(default)]
    bri: Option<u8>,
    #[serde(default)]
    hue: Option<u16>,
    #[serde(default)]
    sat: Option<u8>,
    #[serde(default)]
    xy: Option<[f64; 2]>,
    #[serde(default)]
    ct: Option<u16>,
    #[serde(default)]
    colormode: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct WatchState {
    last_holder_count: u64,
    pending_pulses: u64,
    #[serde(default)]
    holder_light_state: Option<HueState>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let command = env::args().nth(1).unwrap_or_else(|| "watch".to_owned());
    let client = Client::builder()
        .user_agent("potato-hue/0.1")
        .build()
        .context("could not create HTTP client")?;

    match command.as_str() {
        "authorize" => authorize(&client).await,
        "list-lights" => list_lights(&client, &Config::for_listing()?).await,
        "test-pulse" => {
            let config = Config::from_env()?;
            pulse_batch(&client, &config, 1).await
        }
        "watch" => watch(&client, &Config::from_env()?).await,
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => {
            print_usage();
            bail!("unknown command: {other}")
        }
    }
}

impl Config {
    fn from_env() -> Result<Self> {
        Self::from_env_with_light_id(true)
    }

    fn for_listing() -> Result<Self> {
        Self::from_env_with_light_id(false)
    }

    fn from_env_with_light_id(require_light_id: bool) -> Result<Self> {
        let pulse_temperature = optional_env_u64("PULSE_TEMPERATURE_K")?;
        let pulse_color = env::var("PULSE_COLOR")
            .ok()
            .filter(|value| !value.trim().is_empty());
        if pulse_temperature.is_some() && pulse_color.is_some() {
            bail!("set only one of PULSE_TEMPERATURE_K and PULSE_COLOR")
        }
        let pulse_style = match (pulse_temperature, pulse_color) {
            (Some(kelvin), None) => PulseStyle::Temperature(kelvin_to_mired(kelvin)?),
            (None, Some(hex)) => {
                let [x, y] = rgb_hex_to_xy(&hex)?;
                PulseStyle::Xy { x, y }
            }
            (None, None) => PulseStyle::Preserve,
            (Some(_), Some(_)) => unreachable!("configuration conflict checked above"),
        };
        let holder = optional_env("WATCHED_ADDRESS")
            .map(|address| HolderConfig::from_env(address.trim().to_owned()))
            .transpose()?;
        Ok(Self {
            bridge_ip: required_env("HUE_BRIDGE_IP")?,
            app_key: required_env("HUE_APP_KEY")?,
            light_id: if require_light_id {
                required_env("HUE_LIGHT_ID")?
            } else {
                env::var("HUE_LIGHT_ID").unwrap_or_default()
            },
            poll_every: Duration::from_secs(env_u64("POTATO_POLL_SECONDS", 60)?),
            pulse_on: Duration::from_millis(env_u64("PULSE_ON_MS", 450)?),
            pulse_gap: Duration::from_millis(env_u64("PULSE_GAP_MS", 180)?),
            pulse_bri: hue_brightness(env_u64("PULSE_MAX_BRIGHTNESS", 100)?)?,
            pulse_style,
            holder,
            state_file: PathBuf::from(
                env::var("POTATO_STATE_FILE")
                    .unwrap_or_else(|_| "potato-hue-state.json".to_owned()),
            ),
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!("http://{}/api/{}{}", self.bridge_ip, self.app_key, path)
    }
}

impl HolderConfig {
    fn from_env(watched_address: String) -> Result<Self> {
        Ok(Self {
            watched_address,
            start_bri: hue_brightness_named("HOLDER_START_BRIGHTNESS", 10)?,
            end_bri: hue_brightness_named("HOLDER_END_BRIGHTNESS", 100)?,
            start_xy: rgb_hex_to_xy(
                &optional_env("HOLDER_START_COLOR").unwrap_or_else(|| "#8B4513".to_owned()),
            )?,
            end_xy: rgb_hex_to_xy(
                &optional_env("HOLDER_END_COLOR").unwrap_or_else(|| "#FF0000".to_owned()),
            )?,
            start_ct: kelvin_to_mired_named(
                "HOLDER_START_TEMPERATURE_K",
                optional_env_u64("HOLDER_START_TEMPERATURE_K")?.unwrap_or(2_200),
            )?,
            end_ct: kelvin_to_mired_named(
                "HOLDER_END_TEMPERATURE_K",
                optional_env_u64("HOLDER_END_TEMPERATURE_K")?.unwrap_or(6_500),
            )?,
        })
    }
}

fn required_env(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("{name} is required; see .env.example"))
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    match env::var(name) {
        Ok(value) => value
            .parse()
            .with_context(|| format!("{name} must be a positive integer")),
        Err(_) => Ok(default),
    }
}

fn optional_env_u64(name: &str) -> Result<Option<u64>> {
    match env::var(name).ok().filter(|value| !value.trim().is_empty()) {
        Some(value) => value
            .parse()
            .map(Some)
            .with_context(|| format!("{name} must be a positive integer")),
        None => Ok(None),
    }
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn kelvin_to_mired(kelvin: u64) -> Result<u16> {
    kelvin_to_mired_named("PULSE_TEMPERATURE_K", kelvin)
}

fn kelvin_to_mired_named(name: &str, kelvin: u64) -> Result<u16> {
    if kelvin == 0 {
        bail!("{name} must be greater than zero")
    }
    let mired = 1_000_000 / kelvin;
    if !(153..=500).contains(&mired) {
        bail!("{name} must be between 2000K and 6535K for a Hue colour-temperature light")
    }
    Ok(mired as u16)
}

fn hue_brightness(percent: u64) -> Result<u8> {
    hue_brightness_value("PULSE_MAX_BRIGHTNESS", percent)
}

fn hue_brightness_named(name: &str, default: u64) -> Result<u8> {
    hue_brightness_value(name, env_u64(name, default)?)
}

fn hue_brightness_value(name: &str, percent: u64) -> Result<u8> {
    if !(1..=100).contains(&percent) {
        bail!("{name} must be a percentage from 1 to 100")
    }
    // Hue's v1 API has a brightness range of 1–254. Round so 100% means 254.
    Ok(((percent * 254 + 50) / 100) as u8)
}

fn rgb_hex_to_xy(value: &str) -> Result<[f64; 2]> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("PULSE_COLOR must be a six-digit RGB value, for example #FF9A20")
    }
    let red = u8::from_str_radix(&hex[0..2], 16)?;
    let green = u8::from_str_radix(&hex[2..4], 16)?;
    let blue = u8::from_str_radix(&hex[4..6], 16)?;
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value > 0.04045 {
            ((value + 0.055) / 1.055).powf(2.4)
        } else {
            value / 12.92
        }
    };
    let (red, green, blue) = (linear(red), linear(green), linear(blue));
    // sRGB (D65) to CIE 1931 XYZ, then to chromaticity coordinates accepted by Hue.
    let x = red * 0.664_511 + green * 0.154_324 + blue * 0.162_028;
    let y = red * 0.283_881 + green * 0.668_433 + blue * 0.047_685;
    let z = red * 0.000_088 + green * 0.072_310 + blue * 0.986_039;
    let total = x + y + z;
    if total <= f64::EPSILON {
        bail!("PULSE_COLOR cannot be black; use PULSE_MAX_BRIGHTNESS to choose pulse brightness")
    }
    Ok([x / total, y / total])
}

async fn authorize(client: &Client) -> Result<()> {
    let bridge_ip = required_env("HUE_BRIDGE_IP")?;
    let device_type = env::var("HUE_DEVICE_TYPE").unwrap_or_else(|_| "potato-hue#host".to_owned());
    if device_type.len() > 40 || !device_type.contains('#') {
        bail!("HUE_DEVICE_TYPE must look like app-name#device-name and be at most 40 characters")
    }

    println!(
        "Press the physical button on the Hue bridge, then run this command within 30 seconds."
    );
    let response = client
        .post(format!("http://{bridge_ip}/api"))
        .json(&json!({ "devicetype": device_type }))
        .send()
        .await
        .context("could not reach the Hue bridge")?
        .error_for_status()
        .context("Hue bridge returned an HTTP error")?
        .json::<Value>()
        .await
        .context("Hue bridge returned invalid JSON")?;

    if let Some(username) = response
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item.get("success"))
        .and_then(|success| success.get("username"))
        .and_then(Value::as_str)
    {
        println!("\nAuthorization complete. Add this to .env:\nHUE_APP_KEY={username}");
        return Ok(());
    }

    bail!("Hue authorization failed: {}", hue_error_message(&response))
}

async fn list_lights(client: &Client, config: &Config) -> Result<()> {
    let lights: HashMap<String, HueLight> = hue_get(client, config, "/lights").await?;
    if lights.is_empty() {
        println!("No lights were returned by the bridge.");
        return Ok(());
    }

    println!("Available lights:");
    let mut lights: Vec<_> = lights.into_iter().collect();
    lights.sort_by(|(first, _), (second, _)| first.cmp(second));
    for (id, light) in lights {
        let kind = light.productname.as_deref().unwrap_or("Hue light");
        let colour_support = if light.capabilities.control.colorgamut.is_some() {
            "RGB"
        } else if light.capabilities.control.ct.is_some() {
            "white temperature"
        } else {
            "brightness only"
        };
        println!(
            "  {id:>3}  {:<28} {kind}; {colour_support} ({})",
            light.name,
            if light.state.on { "on" } else { "off" }
        );
    }
    println!("\nSet HUE_LIGHT_ID to the id of the chosen light in .env.");
    Ok(())
}

async fn watch(client: &Client, config: &Config) -> Result<()> {
    let mut watch_state = load_watch_state(&config.state_file)?;
    let mut potato_state = fetch_potato_state(client).await?;

    if !config.state_file.exists() {
        watch_state.last_holder_count = potato_state.holder_count;
        save_watch_state(&config.state_file, &watch_state)?;
        println!(
            "Watching from holder count {}; existing snatches will not be replayed.",
            potato_state.holder_count
        );
    } else {
        println!(
            "Watching Pot Potato (last count {}, {} pulse(s) queued).",
            watch_state.last_holder_count, watch_state.pending_pulses
        );
    }

    loop {
        if let Err(error) =
            update_holder_mode(client, config, &potato_state, &mut watch_state).await
        {
            eprintln!("Holder mode update failed; retrying: {error:#}");
        }

        if watch_state.pending_pulses > 0 {
            let pulses = watch_state.pending_pulses;
            println!("Pulsing {pulses} time(s)…");
            match pulse_batch(client, config, pulses).await {
                Ok(()) => {
                    watch_state.pending_pulses = 0;
                    save_watch_state(&config.state_file, &watch_state)?;
                }
                Err(error) => eprintln!("Pulse failed; it will be retried: {error:#}"),
            }
        }

        sleep(config.poll_every).await;
        match fetch_potato_state(client).await {
            Ok(next_state) if next_state.holder_count > watch_state.last_holder_count => {
                let added = next_state.holder_count - watch_state.last_holder_count;
                watch_state.last_holder_count = next_state.holder_count;
                watch_state.pending_pulses += added;
                save_watch_state(&config.state_file, &watch_state)?;
                println!(
                    "Snatched {added} time(s); {} pulse(s) queued.",
                    watch_state.pending_pulses
                );
                potato_state = next_state;
            }
            Ok(next_state) if next_state.holder_count < watch_state.last_holder_count => {
                // A new game or a reset: adopt its count without replaying an arbitrary number of pulses.
                watch_state.last_holder_count = next_state.holder_count;
                save_watch_state(&config.state_file, &watch_state)?;
                println!(
                    "Holder count moved backwards; reset baseline to {}.",
                    next_state.holder_count
                );
                potato_state = next_state;
            }
            Ok(next_state) => potato_state = next_state,
            Err(error) => eprintln!("Pot Potato check failed; retrying: {error:#}"),
        }
    }
}

async fn fetch_potato_state(client: &Client) -> Result<PotatoState> {
    let response = client
        .get(POTATO_STATE_URL)
        .send()
        .await
        .context("could not reach Pot Potato")?
        .error_for_status()
        .context("Pot Potato returned an HTTP error")?
        .json::<PotatoResponse>()
        .await
        .context("Pot Potato state response was invalid")?;
    Ok(response.state)
}

async fn update_holder_mode(
    client: &Client,
    config: &Config,
    potato: &PotatoState,
    watch_state: &mut WatchState,
) -> Result<()> {
    let is_watched_holder = config.holder.as_ref().is_some_and(|holder| {
        potato
            .current_owner
            .as_deref()
            .is_some_and(|owner| owner.eq_ignore_ascii_case(&holder.watched_address))
    });

    if !is_watched_holder {
        if let Some(saved_state) = watch_state.holder_light_state.take() {
            put_light_state(client, config, restore_command(&saved_state)).await?;
            save_watch_state(&config.state_file, watch_state)?;
            println!("Watched address no longer holds the potato; restored the light.");
        }
        return Ok(());
    }

    let holder = config.holder.as_ref().expect("checked above");
    let light: HueLight = hue_get(client, config, &format!("/lights/{}", config.light_id)).await?;
    if watch_state.holder_light_state.is_none() {
        watch_state.holder_light_state = Some(light.state.clone());
        save_watch_state(&config.state_file, watch_state)?;
        println!("Watched address holds the potato; starting the hot-potato light.");
    }
    let progress = hold_progress(potato, current_time_ms());
    put_light_state(
        client,
        config,
        holder_command(holder, &light, progress, holder_transition_time(config)),
    )
    .await
}

fn current_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn hold_progress(potato: &PotatoState, now_ms: i64) -> f64 {
    let (Some(start), Some(deadline)) = (potato.purchased_at, potato.deadline) else {
        return 0.0;
    };
    if deadline <= start {
        return 0.0;
    }
    ((now_ms - start) as f64 / (deadline - start) as f64).clamp(0.0, 1.0)
}

fn holder_transition_time(config: &Config) -> u16 {
    // Hue measures transition time in deciseconds. A short transition keeps each
    // 60-second status update perceptibly smooth without delaying a restore.
    (config.poll_every.as_millis() / 100).clamp(1, 50) as u16
}

fn holder_command(
    holder: &HolderConfig,
    light: &HueLight,
    progress: f64,
    transitiontime: u16,
) -> Value {
    let mut command = Map::new();
    command.insert("on".to_owned(), Value::Bool(true));
    command.insert(
        "bri".to_owned(),
        Value::from(interpolate_u8(holder.start_bri, holder.end_bri, progress)),
    );
    command.insert("transitiontime".to_owned(), Value::from(transitiontime));

    if light.capabilities.control.colorgamut.is_some() {
        let [x, y] = interpolate_xy(holder.start_xy, holder.end_xy, progress);
        command.insert("xy".to_owned(), json!([x, y]));
    } else if let Some(range) = light.capabilities.control.ct {
        let ct =
            interpolate_u16(holder.start_ct, holder.end_ct, progress).clamp(range.min, range.max);
        command.insert("ct".to_owned(), Value::from(ct));
    }
    Value::Object(command)
}

fn interpolate_u8(start: u8, end: u8, progress: f64) -> u8 {
    (f64::from(start) + (f64::from(end) - f64::from(start)) * progress)
        .round()
        .clamp(0.0, f64::from(u8::MAX)) as u8
}

fn interpolate_u16(start: u16, end: u16, progress: f64) -> u16 {
    (f64::from(start) + (f64::from(end) - f64::from(start)) * progress)
        .round()
        .clamp(0.0, f64::from(u16::MAX)) as u16
}

fn interpolate_xy(start: [f64; 2], end: [f64; 2], progress: f64) -> [f64; 2] {
    [
        start[0] + (end[0] - start[0]) * progress,
        start[1] + (end[1] - start[1]) * progress,
    ]
}

async fn pulse_batch(client: &Client, config: &Config, count: u64) -> Result<()> {
    let light: HueLight = hue_get(client, config, &format!("/lights/{}", config.light_id)).await?;
    let original = light.state;
    println!("Pulsing {} ({count} time(s)).", light.name);

    let result = async {
        if original.on {
            put_light_state(client, config, json!({ "on": false, "transitiontime": 0 })).await?;
        }

        for pulse in 0..count {
            put_light_state(client, config, pulse_command(config)).await?;
            sleep(config.pulse_on).await;
            put_light_state(client, config, json!({ "on": false, "transitiontime": 0 })).await?;
            if pulse + 1 < count {
                sleep(config.pulse_gap).await;
            }
        }
        Ok(())
    }
    .await;

    // Always make a best effort to return to the state captured before the batch.
    let restore_result = put_light_state(client, config, restore_command(&original)).await;
    match (result, restore_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(pulse_error), Ok(())) => Err(pulse_error),
        (Ok(()), Err(restore_error)) => Err(restore_error),
        (Err(pulse_error), Err(restore_error)) => Err(anyhow!(
            "pulse error: {pulse_error:#}; also could not restore the light: {restore_error:#}"
        )),
    }
}

fn pulse_command(config: &Config) -> Value {
    let mut command = Map::new();
    command.insert("on".to_owned(), Value::Bool(true));
    command.insert("bri".to_owned(), Value::from(config.pulse_bri));
    command.insert("transitiontime".to_owned(), Value::from(0));
    match config.pulse_style {
        PulseStyle::Preserve => {}
        PulseStyle::Temperature(ct) => {
            command.insert("ct".to_owned(), Value::from(ct));
        }
        PulseStyle::Xy { x, y } => {
            command.insert("xy".to_owned(), json!([x, y]));
        }
    }
    Value::Object(command)
}

fn restore_command(state: &HueState) -> Value {
    let mut command = Map::new();
    command.insert("on".to_owned(), Value::Bool(state.on));
    command.insert("transitiontime".to_owned(), Value::from(0));
    if !state.on {
        return Value::Object(command);
    }

    if let Some(bri) = state.bri {
        command.insert("bri".to_owned(), Value::from(bri));
    }
    match state.colormode.as_deref() {
        Some("xy") => {
            if let Some([x, y]) = state.xy {
                command.insert("xy".to_owned(), json!([x, y]));
            }
        }
        Some("ct") => {
            if let Some(ct) = state.ct {
                command.insert("ct".to_owned(), Value::from(ct));
            }
        }
        Some("hs") => {
            if let Some(hue) = state.hue {
                command.insert("hue".to_owned(), Value::from(hue));
            }
            if let Some(sat) = state.sat {
                command.insert("sat".to_owned(), Value::from(sat));
            }
        }
        _ => {}
    }
    Value::Object(command)
}

async fn hue_get<T: for<'de> Deserialize<'de>>(
    client: &Client,
    config: &Config,
    path: &str,
) -> Result<T> {
    let response = client
        .get(config.api_url(path))
        .send()
        .await
        .context("could not reach the Hue bridge")?
        .error_for_status()
        .context("Hue bridge returned an HTTP error")?
        .json::<Value>()
        .await
        .context("Hue bridge returned invalid JSON")?;
    if response_is_hue_error(&response) {
        bail!("Hue request failed: {}", hue_error_message(&response));
    }
    serde_json::from_value(response).context("Hue bridge returned an unexpected response")
}

async fn put_light_state(client: &Client, config: &Config, command: Value) -> Result<()> {
    let response = client
        .put(config.api_url(&format!("/lights/{}/state", config.light_id)))
        .json(&command)
        .send()
        .await
        .context("could not send command to the Hue bridge")?
        .error_for_status()
        .context("Hue bridge returned an HTTP error")?
        .json::<Value>()
        .await
        .context("Hue bridge returned invalid JSON")?;
    if response_is_hue_error(&response) {
        bail!("Hue command failed: {}", hue_error_message(&response));
    }
    Ok(())
}

fn response_is_hue_error(response: &Value) -> bool {
    response
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item.get("error").is_some()))
}

fn hue_error_message(response: &Value) -> String {
    response
        .as_array()
        .and_then(|items| items.iter().find_map(|item| item.get("error")))
        .and_then(|error| error.get("description"))
        .and_then(Value::as_str)
        .unwrap_or("unknown bridge error")
        .to_owned()
}

fn load_watch_state(path: &Path) -> Result<WatchState> {
    if !path.exists() {
        return Ok(WatchState::default());
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("could not read state file {}", path.display()))?;
    serde_json::from_str(&contents)
        .with_context(|| format!("state file {} is invalid", path.display()))
}

fn save_watch_state(path: &Path, state: &WatchState) -> Result<()> {
    let contents = serde_json::to_vec_pretty(state).context("could not encode watch state")?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, contents)
        .with_context(|| format!("could not write state file {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .with_context(|| format!("could not replace state file {}", path.display()))
}

fn print_usage() {
    println!(
        "Usage: potato-hue [authorize | list-lights | test-pulse | watch]\n\
         \n\
         authorize   Create a Hue application key after pressing the bridge button\n\
         list-lights Show light ids and names\n\
         test-pulse  Pulse the selected light once and restore it\n\
         watch       Watch Pot Potato and pulse for every snatch (default)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_an_on_xy_light() {
        let state = HueState {
            on: true,
            bri: Some(100),
            hue: None,
            sat: None,
            xy: Some([0.5, 0.4]),
            ct: None,
            colormode: Some("xy".to_owned()),
        };
        assert_eq!(
            restore_command(&state),
            json!({ "on": true, "transitiontime": 0, "bri": 100, "xy": [0.5, 0.4] })
        );
    }

    #[test]
    fn restores_an_off_light_without_changing_its_colour() {
        let state = HueState {
            on: false,
            bri: Some(150),
            hue: Some(2_000),
            sat: Some(200),
            xy: None,
            ct: None,
            colormode: Some("hs".to_owned()),
        };
        assert_eq!(
            restore_command(&state),
            json!({ "on": false, "transitiontime": 0 })
        );
    }

    #[test]
    fn parses_rgb_colour_for_hue() {
        let [x, y] = rgb_hex_to_xy("#FF0000").unwrap();
        assert!((x - 0.700_606).abs() < 0.000_001);
        assert!((y - 0.299_301).abs() < 0.000_001);
    }

    #[test]
    fn converts_warm_white_temperature_to_mired() {
        assert_eq!(kelvin_to_mired(2_700).unwrap(), 370);
    }

    #[test]
    fn converts_a_brightness_percentage_to_hue_scale() {
        assert_eq!(hue_brightness(100).unwrap(), 254);
        assert_eq!(hue_brightness(50).unwrap(), 127);
        assert!(hue_brightness(0).is_err());
    }

    fn holder() -> HolderConfig {
        HolderConfig {
            watched_address: "xch1test".to_owned(),
            start_bri: 25,
            end_bri: 254,
            start_xy: [0.5, 0.4],
            end_xy: [0.7, 0.3],
            start_ct: 454,
            end_ct: 153,
        }
    }

    fn test_light(control: HueControl) -> HueLight {
        HueLight {
            name: "Test light".to_owned(),
            productname: None,
            state: HueState {
                on: false,
                bri: Some(100),
                hue: None,
                sat: None,
                xy: None,
                ct: None,
                colormode: None,
            },
            capabilities: HueCapabilities { control },
        }
    }

    #[test]
    fn holder_mode_uses_rgb_gradient_for_rgb_lights() {
        let light = test_light(HueControl {
            colorgamut: Some(vec![[0.1, 0.1]]),
            ct: Some(HueTemperatureRange { min: 153, max: 500 }),
        });
        assert_eq!(
            holder_command(&holder(), &light, 0.5, 10),
            json!({ "on": true, "bri": 140, "transitiontime": 10, "xy": [0.6, 0.35] })
        );
    }

    #[test]
    fn holder_mode_uses_and_clamps_temperature_for_white_lights() {
        let light = test_light(HueControl {
            colorgamut: None,
            ct: Some(HueTemperatureRange { min: 200, max: 400 }),
        });
        let command = holder_command(&holder(), &light, 1.0, 10);
        assert_eq!(command["bri"], 254);
        assert_eq!(command["ct"], 200);
        assert!(command.get("xy").is_none());
    }

    #[test]
    fn holder_progress_tracks_the_purchase_window() {
        let potato = PotatoState {
            holder_count: 1,
            current_owner: Some("xch1test".to_owned()),
            purchased_at: Some(1_000),
            deadline: Some(5_000),
        };
        assert_eq!(hold_progress(&potato, 3_000), 0.5);
        assert_eq!(hold_progress(&potato, 10_000), 1.0);
    }
}
