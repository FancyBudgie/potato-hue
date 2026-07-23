use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
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
    state_file: PathBuf,
}

#[derive(Debug, Clone, Copy)]
enum PulseStyle {
    Preserve,
    Temperature(u16),
    Xy { x: f64, y: f64 },
}

#[derive(Debug, Deserialize)]
struct PotatoResponse {
    state: PotatoState,
}

#[derive(Debug, Deserialize)]
struct PotatoState {
    #[serde(rename = "holderCount")]
    holder_count: u64,
}

#[derive(Debug, Deserialize)]
struct HueLight {
    name: String,
    #[serde(default)]
    productname: Option<String>,
    state: HueState,
}

#[derive(Debug, Clone, Deserialize)]
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

fn kelvin_to_mired(kelvin: u64) -> Result<u16> {
    if kelvin == 0 {
        bail!("PULSE_TEMPERATURE_K must be greater than zero")
    }
    let mired = 1_000_000 / kelvin;
    if !(153..=500).contains(&mired) {
        bail!(
            "PULSE_TEMPERATURE_K must be between 2000K and 6535K for a Hue colour-temperature light"
        )
    }
    Ok(mired as u16)
}

fn hue_brightness(percent: u64) -> Result<u8> {
    if !(1..=100).contains(&percent) {
        bail!("PULSE_MAX_BRIGHTNESS must be a percentage from 1 to 100")
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
        bail!("PULSE_COLOR cannot be black; use PULSE_BRI to choose pulse brightness")
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
        println!(
            "  {id:>3}  {:<28} {kind} ({})",
            light.name,
            if light.state.on { "on" } else { "off" }
        );
    }
    println!("\nSet HUE_LIGHT_ID to the id of the chosen light in .env.");
    Ok(())
}

async fn watch(client: &Client, config: &Config) -> Result<()> {
    let mut watch_state = load_watch_state(&config.state_file)?;
    let initial_count = holder_count(client).await?;

    if !config.state_file.exists() {
        watch_state.last_holder_count = initial_count;
        save_watch_state(&config.state_file, &watch_state)?;
        println!(
            "Watching from holder count {initial_count}; existing snatches will not be replayed."
        );
    } else {
        println!(
            "Watching Pot Potato (last count {}, {} pulse(s) queued).",
            watch_state.last_holder_count, watch_state.pending_pulses
        );
    }

    loop {
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
        match holder_count(client).await {
            Ok(current_count) if current_count > watch_state.last_holder_count => {
                let added = current_count - watch_state.last_holder_count;
                watch_state.last_holder_count = current_count;
                watch_state.pending_pulses += added;
                save_watch_state(&config.state_file, &watch_state)?;
                println!(
                    "Snatched {added} time(s); {} pulse(s) queued.",
                    watch_state.pending_pulses
                );
            }
            Ok(current_count) if current_count < watch_state.last_holder_count => {
                // A new game or a reset: adopt its count without replaying an arbitrary number of pulses.
                watch_state.last_holder_count = current_count;
                save_watch_state(&config.state_file, &watch_state)?;
                println!("Holder count moved backwards; reset baseline to {current_count}.");
            }
            Ok(_) => {}
            Err(error) => eprintln!("Pot Potato check failed; retrying: {error:#}"),
        }
    }
}

async fn holder_count(client: &Client) -> Result<u64> {
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
    Ok(response.state.holder_count)
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
}
