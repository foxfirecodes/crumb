use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{interval_at, sleep, Instant, MissedTickBehavior};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const DISCORD_API: &str = "https://discord.com/api/v10";
const DISCORD_GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
const USER_AGENT_VALUE: &str = "Crumb/0.1";

#[derive(Debug, Clone)]
pub struct NormalizedPerson {
    pub id: String,
    pub key: String,
    pub display_name: String,
    pub username: String,
}

#[derive(Debug, Clone)]
pub struct NormalizedMessage {
    pub id: String,
    pub author: String,
    pub author_key: String,
    pub author_username: String,
    pub content: String,
    pub timestamp: String,
    pub reply_to_id: Option<String>,
    pub attachments: Vec<String>,
    pub embeds: Vec<String>,
    pub components: Vec<String>,
    pub mentions: Vec<NormalizedPerson>,
}

#[derive(Debug)]
pub struct ScrapeRequest {
    pub scrape_id: String,
    pub channel_id: String,
    pub channel_name: Option<String>,
    pub guild_id: Option<String>,
    pub guild_name: Option<String>,
    pub triggered_by: String,
    pub limit: usize,
    pub reply: InteractionReply,
}

#[derive(Debug)]
pub struct WatchRequest {
    pub interaction_id: String,
    pub channel_id: String,
    pub channel_name: Option<String>,
    pub guild_id: Option<String>,
    pub guild_name: Option<String>,
    pub triggered_by: String,
    pub reply: InteractionReply,
}

#[derive(Debug)]
pub enum DiscordCommand {
    Scrape(ScrapeRequest),
    Watch(WatchRequest),
    Unwatch(WatchRequest),
}

#[derive(Debug)]
pub struct BotReady {
    pub bot_user: Option<String>,
}

#[derive(Clone)]
pub struct DiscordBot {
    http: reqwest::Client,
    app_id: String,
    token: String,
}

impl DiscordBot {
    pub fn new(app_id: String, token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            app_id,
            token,
        }
    }

    pub async fn register_commands(&self) -> Result<()> {
        let url = format!("{DISCORD_API}/applications/{}/commands", self.app_id);
        let body = application_command_definitions();

        let response = self
            .http
            .get(&url)
            .header(AUTHORIZATION, bot_auth(&self.token))
            .header(USER_AGENT, USER_AGENT_VALUE)
            .send()
            .await
            .context("fetching Discord commands")?;
        let existing: Value = parse_json_response(response)
            .await
            .context("Discord command fetch failed")?;
        if application_commands_match(&existing, &body) {
            tracing::info!("Discord commands already up to date");
            return Ok(());
        }

        let response = self
            .http
            .put(&url)
            .header(AUTHORIZATION, bot_auth(&self.token))
            .header(USER_AGENT, USER_AGENT_VALUE)
            .json(&body)
            .send()
            .await
            .context("registering Discord command")?;

        expect_success(response)
            .await
            .context("Discord command registration failed")?;
        tracing::info!("registered Discord commands");
        Ok(())
    }

    pub async fn run(
        self,
        command_tx: mpsc::UnboundedSender<DiscordCommand>,
        shutdown: watch::Receiver<bool>,
        ready_tx: oneshot::Sender<BotReady>,
    ) {
        let mut ready_tx = Some(ready_tx);
        let mut shutdown = shutdown;

        loop {
            if *shutdown.borrow() {
                return;
            }

            let result = self
                .run_gateway_once(command_tx.clone(), &mut shutdown, &mut ready_tx)
                .await;

            if *shutdown.borrow() {
                return;
            }

            if let Err(e) = result {
                tracing::warn!("Discord gateway disconnected: {e}");
            }

            tokio::select! {
                _ = shutdown.changed() => return,
                _ = sleep(Duration::from_secs(5)) => {}
            }
        }
    }

    async fn run_gateway_once(
        &self,
        command_tx: mpsc::UnboundedSender<DiscordCommand>,
        shutdown: &mut watch::Receiver<bool>,
        ready_tx: &mut Option<oneshot::Sender<BotReady>>,
    ) -> Result<()> {
        let (ws, _) = connect_async(DISCORD_GATEWAY)
            .await
            .context("connecting Discord gateway")?;
        let (mut write, mut read) = ws.split();
        let mut seq: Option<u64> = None;
        let mut heartbeat_enabled = false;
        let mut heartbeat = interval_at(
            Instant::now() + Duration::from_secs(60),
            Duration::from_secs(60),
        );
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    let _ = changed;
                    let _ = write.close().await;
                    return Ok(());
                }
                _ = heartbeat.tick(), if heartbeat_enabled => {
                    write
                        .send(Message::Text(json!({"op": 1, "d": seq}).to_string().into()))
                        .await
                        .context("sending Discord heartbeat")?;
                }
                next = read.next() => {
                    let Some(next) = next else {
                        bail!("Discord gateway stream ended");
                    };
                    let message = next.context("reading Discord gateway")?;
                    match message {
                        Message::Text(text) => {
                            let payload: GatewayPayload = serde_json::from_str(&text)
                                .with_context(|| format!("parsing Discord gateway payload: {text}"))?;
                            if let Some(s) = payload.s {
                                seq = Some(s);
                            }
                            match payload.op {
                                0 => {
                                    self.handle_dispatch(payload, &command_tx, ready_tx).await?;
                                }
                                1 => {
                                    write
                                        .send(Message::Text(json!({"op": 1, "d": seq}).to_string().into()))
                                        .await
                                        .context("responding to Discord heartbeat request")?;
                                }
                                7 => bail!("Discord requested reconnect"),
                                9 => bail!("Discord invalidated the session"),
                                10 => {
                                    let hello: HelloEvent = serde_json::from_value(payload.d)
                                        .context("parsing Discord hello")?;
                                    let interval = Duration::from_millis(hello.heartbeat_interval);
                                    heartbeat = interval_at(Instant::now() + interval, interval);
                                    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
                                    heartbeat_enabled = true;

                                    let identify = json!({
                                        "op": 2,
                                        "d": {
                                            "token": self.token,
                                            "intents": 1,
                                            "properties": {
                                                "os": std::env::consts::OS,
                                                "browser": "crumb",
                                                "device": "crumb"
                                            }
                                        }
                                    });
                                    write
                                        .send(Message::Text(identify.to_string().into()))
                                        .await
                                        .context("identifying Discord gateway")?;
                                }
                                11 => {}
                                other => tracing::debug!("ignored Discord gateway op {other}"),
                            }
                        }
                        Message::Ping(bytes) => {
                            write.send(Message::Pong(bytes)).await.context("sending pong")?;
                        }
                        Message::Close(frame) => {
                            bail!("Discord gateway closed: {frame:?}");
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    async fn handle_dispatch(
        &self,
        payload: GatewayPayload,
        command_tx: &mpsc::UnboundedSender<DiscordCommand>,
        ready_tx: &mut Option<oneshot::Sender<BotReady>>,
    ) -> Result<()> {
        match payload.t.as_deref() {
            Some("READY") => {
                let ready: ReadyEvent =
                    serde_json::from_value(payload.d).context("parsing READY")?;
                let bot_user = Some(format_user_tag(&ready.user));
                tracing::info!("bot ready as {}", bot_user.as_deref().unwrap_or("unknown"));
                if let Some(tx) = ready_tx.take() {
                    let _ = tx.send(BotReady { bot_user });
                }
            }
            Some("INTERACTION_CREATE") => {
                let interaction: InteractionCreate =
                    serde_json::from_value(payload.d).context("parsing INTERACTION_CREATE")?;
                if interaction.kind != 2
                    || !matches!(
                        interaction.data.name.as_str(),
                        "scrape" | "watch" | "unwatch"
                    )
                {
                    return Ok(());
                }

                if let Err(e) = self.defer_reply(&interaction).await {
                    tracing::warn!("failed to defer /scrape reply: {e}");
                }

                let user = interaction
                    .user
                    .as_ref()
                    .or_else(|| interaction.member.as_ref().map(|m| &m.user))
                    .map(format_user_tag)
                    .unwrap_or_else(|| "unknown".into());

                let channel_id = interaction.channel_id;
                let channel_name = match format_interaction_channel_name(
                    interaction.channel.as_ref(),
                    interaction.guild_id.as_ref(),
                ) {
                    Some(name) => Some(name),
                    None => self
                        .fetch_channel_name(&channel_id, interaction.guild_id.as_ref())
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!("failed to fetch channel metadata: {e}");
                            None
                        }),
                };
                let guild_id = interaction.guild_id;
                let guild_name = interaction.guild.and_then(|g| g.name);
                let reply = InteractionReply {
                    http: self.http.clone(),
                    app_id: self.app_id.clone(),
                    interaction_token: interaction.token,
                };

                let command = match interaction.data.name.as_str() {
                    "scrape" => {
                        let limit = interaction
                            .data
                            .options
                            .iter()
                            .find(|opt| opt.name == "limit")
                            .and_then(|opt| opt.value.as_i64())
                            .unwrap_or(200)
                            .clamp(1, 1000) as usize;
                        DiscordCommand::Scrape(ScrapeRequest {
                            scrape_id: format!("discord:{channel_id}"),
                            channel_id,
                            channel_name,
                            guild_id,
                            guild_name,
                            triggered_by: user,
                            limit,
                            reply,
                        })
                    }
                    "watch" => DiscordCommand::Watch(WatchRequest {
                        interaction_id: interaction.id,
                        channel_id,
                        channel_name,
                        guild_id,
                        guild_name,
                        triggered_by: user,
                        reply,
                    }),
                    "unwatch" => DiscordCommand::Unwatch(WatchRequest {
                        interaction_id: interaction.id,
                        channel_id,
                        channel_name,
                        guild_id,
                        guild_name,
                        triggered_by: user,
                        reply,
                    }),
                    _ => return Ok(()),
                };

                command_tx
                    .send(command)
                    .map_err(|_| anyhow!("scrape runtime is not accepting requests"))?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn defer_reply(&self, interaction: &InteractionCreate) -> Result<()> {
        let url = format!(
            "{DISCORD_API}/interactions/{}/{}/callback",
            interaction.id, interaction.token
        );
        let response = self
            .http
            .post(url)
            .header(USER_AGENT, USER_AGENT_VALUE)
            .json(&json!({
                "type": 5,
                "data": { "flags": 64 }
            }))
            .send()
            .await
            .context("deferring Discord interaction")?;

        expect_success(response).await
    }

    async fn fetch_channel_name(
        &self,
        channel_id: &str,
        guild_id: Option<&String>,
    ) -> Result<Option<String>> {
        let url = format!("{DISCORD_API}/channels/{channel_id}");
        let response = self
            .http
            .get(url)
            .header(AUTHORIZATION, bot_auth(&self.token))
            .header(USER_AGENT, USER_AGENT_VALUE)
            .send()
            .await
            .context("fetching Discord channel metadata")?;
        let channel: PartialChannel = parse_json_response(response).await?;
        Ok(format_interaction_channel_name(Some(&channel), guild_id))
    }
}

fn application_command_definitions() -> Value {
    json!([
        {
            "type": 1,
            "name": "scrape",
            "description": "Pull recent messages from this channel and extract decisions + action items.",
            "options": [
                {
                    "type": 4,
                    "name": "limit",
                    "description": "How many recent messages to scrape (1-1000, default 200)",
                    "min_value": 1,
                    "max_value": 1000,
                    "required": false
                }
            ],
            "integration_types": [1, 0],
            "contexts": [0, 1, 2]
        },
        {
            "type": 1,
            "name": "watch",
            "description": "Watch this channel for new action items every few minutes.",
            "integration_types": [1, 0],
            "contexts": [0, 1, 2]
        },
        {
            "type": 1,
            "name": "unwatch",
            "description": "Stop watching this channel for new action items.",
            "integration_types": [1, 0],
            "contexts": [0, 1, 2]
        }
    ])
}

fn application_commands_match(existing: &Value, desired: &Value) -> bool {
    comparable_application_commands(existing) == comparable_application_commands(desired)
}

fn comparable_application_commands(value: &Value) -> Option<Vec<Value>> {
    let mut commands = value
        .as_array()?
        .iter()
        .filter_map(comparable_application_command)
        .collect::<Vec<_>>();
    commands.sort_by_key(command_name);
    Some(commands)
}

fn comparable_application_command(command: &Value) -> Option<Value> {
    let object = command.as_object()?;
    let mut options = object
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(comparable_application_command_option)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    options.sort_by_key(command_name);

    Some(json!({
        "type": object.get("type").cloned().unwrap_or(Value::Null),
        "name": object.get("name").cloned().unwrap_or(Value::Null),
        "description": object.get("description").cloned().unwrap_or(Value::Null),
        "integration_types": sorted_values(object.get("integration_types")),
        "contexts": sorted_values(object.get("contexts")),
        "options": options
    }))
}

fn comparable_application_command_option(option: &Value) -> Option<Value> {
    let object = option.as_object()?;
    Some(json!({
        "type": object.get("type").cloned().unwrap_or(Value::Null),
        "name": object.get("name").cloned().unwrap_or(Value::Null),
        "description": object.get("description").cloned().unwrap_or(Value::Null),
        "required": object.get("required").cloned().unwrap_or(Value::Bool(false)),
        "min_value": object.get("min_value").cloned().unwrap_or(Value::Null),
        "max_value": object.get("max_value").cloned().unwrap_or(Value::Null)
    }))
}

fn sorted_values(value: Option<&Value>) -> Value {
    let mut values = value.and_then(Value::as_array).cloned().unwrap_or_default();
    values.sort_by_key(|value| value.to_string());
    Value::Array(values)
}

fn command_name(command: &Value) -> String {
    command
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[derive(Clone, Debug)]
pub struct InteractionReply {
    http: reqwest::Client,
    app_id: String,
    interaction_token: String,
}

impl InteractionReply {
    pub async fn send(&self, content: impl Into<String>) -> Result<()> {
        let url = format!(
            "{DISCORD_API}/webhooks/{}/{}{}",
            self.app_id, self.interaction_token, "/messages/@original"
        );
        let response = self
            .http
            .patch(url)
            .header(USER_AGENT, USER_AGENT_VALUE)
            .json(&json!({ "content": content.into() }))
            .send()
            .await
            .context("editing Discord interaction reply")?;

        expect_success(response).await
    }
}

#[derive(Clone)]
pub struct DiscordScraper {
    http: reqwest::Client,
    token: String,
    self_user: NormalizedPerson,
}

impl DiscordScraper {
    pub async fn connect(token: String) -> Result<Self> {
        let http = reqwest::Client::new();
        let response = http
            .get(format!("{DISCORD_API}/users/@me"))
            .header(AUTHORIZATION, token.as_str())
            .header(USER_AGENT, USER_AGENT_VALUE)
            .send()
            .await
            .context("checking Discord user token")?;
        let self_user = if response.status().is_success() {
            let user: ApiUser = response.json().await.context("parsing Discord user")?;
            normalize_person(&user)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("Discord user token rejected ({status}): {text}");
        };

        Ok(Self {
            http,
            token,
            self_user,
        })
    }

    pub fn user(&self) -> Option<String> {
        Some(self.self_user.display_name.clone())
    }

    pub fn self_user(&self) -> NormalizedPerson {
        self.self_user.clone()
    }

    pub async fn fetch_channel_messages<F>(
        &self,
        channel_id: &str,
        limit: usize,
        mut on_progress: F,
    ) -> Result<Vec<NormalizedMessage>>
    where
        F: FnMut(usize),
    {
        let mut messages: Vec<NormalizedMessage> = Vec::new();
        let mut before: Option<String> = None;
        let mut remaining = limit.clamp(1, 1000);

        while remaining > 0 {
            let batch_size = remaining.min(100);
            let mut query = vec![("limit".to_string(), batch_size.to_string())];
            if let Some(before) = before.as_ref() {
                query.push(("before".into(), before.clone()));
            }

            let url = format!("{DISCORD_API}/channels/{channel_id}/messages");
            let mut attempts = 0;
            let batch: Vec<ApiMessage> = loop {
                attempts += 1;
                let response = self
                    .http
                    .get(&url)
                    .header(AUTHORIZATION, self.token.as_str())
                    .header(USER_AGENT, USER_AGENT_VALUE)
                    .query(&query)
                    .send()
                    .await
                    .context("fetching Discord messages")?;

                if response.status().as_u16() == 429 {
                    let retry: RateLimit = response
                        .json()
                        .await
                        .context("parsing Discord rate limit")?;
                    if attempts >= 3 {
                        bail!("Discord rate limited request; retry the scrape in a moment");
                    }
                    sleep(Duration::from_secs_f64(retry.retry_after.max(0.25))).await;
                    continue;
                }

                break parse_json_response(response).await?;
            };

            if batch.is_empty() {
                break;
            }

            before = batch.last().map(|m| m.id.clone());
            remaining -= batch.len();
            messages.extend(batch.into_iter().map(Into::into));
            on_progress(messages.len());

            if messages.len() >= limit || remaining == 0 {
                break;
            }
        }

        messages.reverse();
        Ok(messages)
    }

    pub async fn fetch_channel_messages_after<F>(
        &self,
        channel_id: &str,
        after_message_id: &str,
        limit: usize,
        mut on_progress: F,
    ) -> Result<Vec<NormalizedMessage>>
    where
        F: FnMut(usize),
    {
        let query = vec![
            ("limit".to_string(), limit.clamp(1, 100).to_string()),
            ("after".into(), after_message_id.to_string()),
        ];
        let url = format!("{DISCORD_API}/channels/{channel_id}/messages");
        let mut attempts = 0;
        let batch: Vec<ApiMessage> = loop {
            attempts += 1;
            let response = self
                .http
                .get(&url)
                .header(AUTHORIZATION, self.token.as_str())
                .header(USER_AGENT, USER_AGENT_VALUE)
                .query(&query)
                .send()
                .await
                .context("fetching Discord messages after cursor")?;

            if response.status().as_u16() == 429 {
                let retry: RateLimit = response
                    .json()
                    .await
                    .context("parsing Discord rate limit")?;
                if attempts >= 3 {
                    bail!("Discord rate limited request; retry the scrape in a moment");
                }
                sleep(Duration::from_secs_f64(retry.retry_after.max(0.25))).await;
                continue;
            }

            break parse_json_response(response).await?;
        };

        let mut messages = batch.into_iter().map(Into::into).collect::<Vec<_>>();
        messages.sort_by(|a: &NormalizedMessage, b| compare_message_ids(&a.id, &b.id));
        on_progress(messages.len());
        Ok(messages)
    }
}

#[derive(Debug, Deserialize)]
struct GatewayPayload {
    op: u64,
    #[serde(default)]
    d: Value,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HelloEvent {
    heartbeat_interval: u64,
}

#[derive(Debug, Deserialize)]
struct ReadyEvent {
    user: ApiUser,
}

#[derive(Debug, Deserialize)]
struct InteractionCreate {
    id: String,
    token: String,
    #[serde(rename = "type")]
    kind: u64,
    data: InteractionData,
    channel_id: String,
    #[serde(default)]
    channel: Option<PartialChannel>,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    guild: Option<PartialGuild>,
    #[serde(default)]
    member: Option<InteractionMember>,
    #[serde(default)]
    user: Option<ApiUser>,
}

#[derive(Debug, Deserialize)]
struct InteractionData {
    name: String,
    #[serde(default)]
    options: Vec<InteractionOption>,
}

#[derive(Debug, Deserialize)]
struct InteractionOption {
    name: String,
    value: Value,
}

#[derive(Debug, Deserialize)]
struct PartialChannel {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    recipients: Vec<ApiUser>,
}

#[derive(Debug, Deserialize)]
struct PartialGuild {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InteractionMember {
    user: ApiUser,
}

#[derive(Debug, Deserialize)]
struct ApiUser {
    id: String,
    username: String,
    #[serde(default)]
    discriminator: Option<String>,
    #[serde(default)]
    global_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiMessage {
    id: String,
    author: ApiUser,
    #[serde(default)]
    content: Option<String>,
    timestamp: String,
    #[serde(default)]
    message_reference: Option<MessageReference>,
    #[serde(default)]
    attachments: Vec<ApiAttachment>,
    #[serde(default)]
    embeds: Vec<Value>,
    #[serde(default)]
    components: Vec<Value>,
    #[serde(default)]
    mentions: Vec<ApiUser>,
}

#[derive(Debug, Deserialize)]
struct MessageReference {
    #[serde(default)]
    message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiAttachment {
    url: String,
}

impl From<ApiMessage> for NormalizedMessage {
    fn from(value: ApiMessage) -> Self {
        let author = normalize_person(&value.author);
        Self {
            id: value.id,
            author: author.display_name,
            author_key: author.key,
            author_username: author.username,
            content: value.content.unwrap_or_default(),
            timestamp: value.timestamp,
            reply_to_id: value.message_reference.and_then(|r| r.message_id),
            attachments: value.attachments.into_iter().map(|a| a.url).collect(),
            embeds: value.embeds.iter().filter_map(summarize_embed).collect(),
            components: value
                .components
                .iter()
                .filter_map(summarize_component)
                .collect(),
            mentions: value.mentions.iter().map(normalize_person).collect(),
        }
    }
}

fn format_interaction_channel_name(
    channel: Option<&PartialChannel>,
    guild_id: Option<&String>,
) -> Option<String> {
    let channel = channel?;
    if guild_id.is_none() {
        if !channel.recipients.is_empty() {
            let recipients = channel
                .recipients
                .iter()
                .map(format_user_tag)
                .collect::<Vec<_>>()
                .join(", ");
            return Some(format!("{recipients} (DM)"));
        }
        if let Some(name) = channel.name.as_deref().filter(|name| !name.is_empty()) {
            return Some(format!("{name} (DM)"));
        }
    }
    channel.name.clone()
}

fn normalize_person(user: &ApiUser) -> NormalizedPerson {
    NormalizedPerson {
        id: user.id.clone(),
        key: format!("discord:user:{}", user.id),
        display_name: format_user_tag(user),
        username: user.username.clone(),
    }
}

fn format_user_tag(user: &ApiUser) -> String {
    match user.discriminator.as_deref() {
        Some(discriminator) if discriminator != "0" => {
            format!("{}#{discriminator}", user.username)
        }
        _ => user
            .global_name
            .as_ref()
            .filter(|name| !name.is_empty())
            .cloned()
            .or_else(|| (!user.username.is_empty()).then(|| user.username.clone()))
            .unwrap_or_else(|| user.id.clone()),
    }
}

fn summarize_embed(embed: &Value) -> Option<String> {
    let object = embed.as_object()?;
    let mut parts = Vec::new();
    if let Some(author) = object
        .get("author")
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
    {
        parts.push(author.to_string());
    }
    for key in ["title", "description", "url"] {
        if let Some(value) = object.get(key).and_then(Value::as_str) {
            parts.push(value.to_string());
        }
    }
    if let Some(fields) = object.get("fields").and_then(Value::as_array) {
        for field in fields {
            let Some(field_object) = field.as_object() else {
                continue;
            };
            let name = field_object.get("name").and_then(Value::as_str);
            let value = field_object.get("value").and_then(Value::as_str);
            match (name, value) {
                (Some(name), Some(value)) => parts.push(format!("{name}: {value}")),
                (Some(name), None) => parts.push(name.to_string()),
                (None, Some(value)) => parts.push(value.to_string()),
                (None, None) => {}
            }
        }
    }
    if let Some(footer) = object
        .get("footer")
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
    {
        parts.push(footer.to_string());
    }
    clean_summary(parts.join(" | "))
}

fn summarize_component(component: &Value) -> Option<String> {
    let mut parts = Vec::new();
    collect_component_text(component, &mut parts);
    clean_summary(parts.join(" | "))
}

fn collect_component_text(component: &Value, parts: &mut Vec<String>) {
    if let Some(object) = component.as_object() {
        for key in ["label", "custom_id", "url", "placeholder"] {
            if let Some(value) = object.get(key).and_then(Value::as_str) {
                parts.push(value.to_string());
            }
        }
        if let Some(options) = object.get("options").and_then(Value::as_array) {
            for option in options {
                collect_component_text(option, parts);
            }
        }
        if let Some(children) = object.get("components").and_then(Value::as_array) {
            for child in children {
                collect_component_text(child, parts);
            }
        }
    }
}

fn clean_summary(value: String) -> Option<String> {
    let cleaned = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        None
    } else if cleaned.len() > 1800 {
        Some(format!(
            "{}...",
            cleaned.chars().take(1800).collect::<String>()
        ))
    } else {
        Some(cleaned)
    }
}

fn compare_message_ids(a: &str, b: &str) -> std::cmp::Ordering {
    let parsed = a.parse::<u128>().ok().zip(b.parse::<u128>().ok());
    match parsed {
        Some((a, b)) => a.cmp(&b),
        None => a.cmp(b),
    }
}

fn bot_auth(token: &str) -> String {
    format!("Bot {token}")
}

async fn expect_success(response: reqwest::Response) -> Result<()> {
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    bail!("Discord returned {status}: {text}");
}

async fn parse_json_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T> {
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("Discord returned {status}: {text}");
    }

    response.json().await.context("parsing Discord JSON")
}

#[derive(Debug, Deserialize)]
struct RateLimit {
    retry_after: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_match_ignores_discord_metadata_and_empty_options() {
        let desired = application_command_definitions();
        let mut existing = application_command_definitions();
        let commands = existing.as_array_mut().expect("commands array");
        for (index, command) in commands.iter_mut().enumerate() {
            command["id"] = json!(format!("command-{index}"));
            command["application_id"] = json!("app-id");
            command["version"] = json!("version-id");
            if command.get("options").is_none() {
                command["options"] = json!([]);
            }
        }

        assert!(application_commands_match(&existing, &desired));
    }

    #[test]
    fn command_match_detects_definition_changes() {
        let desired = application_command_definitions();
        let mut existing = application_command_definitions();
        existing.as_array_mut().expect("commands array")[0]["description"] =
            json!("Different scrape description");

        assert!(!application_commands_match(&existing, &desired));
    }

    #[test]
    fn command_match_detects_extra_registered_commands() {
        let desired = application_command_definitions();
        let mut existing = application_command_definitions();
        existing
            .as_array_mut()
            .expect("commands array")
            .push(json!({
                "type": 1,
                "name": "old-command",
                "description": "A command that should be removed.",
                "integration_types": [1, 0],
                "contexts": [0, 1, 2]
            }));

        assert!(!application_commands_match(&existing, &desired));
    }
}
