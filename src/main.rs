use std::{env, fmt};

use serde::{Deserialize, Serialize};
use serenity::model::id::ChannelId;
use serenity::{
    async_trait,
    model::{channel::Message, gateway::Ready},
    prelude::*,
};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::f64::INFINITY;
use std::fmt::{Display, Formatter};
use tokio::fs::File;
use tokio::io::AsyncReadExt;

type Attributes = HashMap<String, AttributeOption>;

struct Handler {
    sensored_channel: ChannelId,
    logging_channel: ChannelId,
    perspective_token: String,
    http_client: reqwest::Client,
    config: Config,
}

#[derive(Serialize, Debug)]
#[allow(non_snake_case)]
struct Request {
    comment: Comment,
    requestedAttributes: Attributes,
    doNotStore: bool,
}

#[derive(Serialize, Debug)]
#[allow(non_snake_case)]
struct Comment {
    text: String,
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
struct AttributeOption {
    #[serde(skip_serializing)]
    threshold: f64,
}

#[derive(Deserialize, Debug)]
struct Emojis {
    passed_check: String,
    failed_check: String,
    passed_check_highest: String,
    passed_check_lowest: String,
    failed_check_highest: String,
    failed_check_lowest: String,
}

#[derive(Deserialize, Debug)]
struct Config {
    emotes: Emojis,
    attributes: Attributes,
}

#[derive(Deserialize, Debug)]
#[allow(non_snake_case)]
struct Response {
    attributeScores: HashMap<String, Score>,
}

#[derive(Deserialize, Debug)]
#[allow(non_snake_case)]
struct Score {
    summaryScore: SummaryScore,
}

#[derive(Deserialize, Debug)]
struct SummaryScore {
    value: f64,
}

#[derive(Debug, PartialEq)]
struct AttributeParsed {
    score: f64,
    rejected: bool,
    name: String,
    emoji: String,
}

impl Display for AttributeParsed {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} `{}` - {:.1}%\n",
            self.emoji,
            self.name,
            self.score * 100f64
        )
    }
}

impl PartialOrd for AttributeParsed {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self.rejected, other.rejected) {
            (true, true) => match self.score.partial_cmp(&other.score) {
                Some(Ordering::Equal) => None,
                etc => etc,
            },
            (false, false) => match self.score.partial_cmp(&other.score) {
                Some(Ordering::Equal) => None,
                etc => etc,
            },
            (true, false) => Some(Ordering::Greater),
            (false, true) => Some(Ordering::Less),
        }
    }
}

impl Ord for AttributeParsed {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap_or(Ordering::Equal)
    }
}

impl Eq for AttributeParsed {}

#[async_trait]
impl EventHandler for Handler {
    // Set a handler for the `message` event - so that whenever a new message
    // is received - the closure (or function) passed will be called.
    //
    // Event handlers are dispatched through a threadpool, and so multiple
    // events can be dispatched simultaneously.
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.channel_id == self.sensored_channel {
            let response: Response = self
                .http_client
                .post(format!(
                    "https://commentanalyzer.googleapis.com/v1alpha1/comments:analyze?key={}",
                    &self.perspective_token
                ))
                .body(
                    serde_json::to_string(&Request {
                        comment: Comment {
                            text: msg.content.clone(),
                        },
                        requestedAttributes: self.config.attributes.clone(),
                        doNotStore: true,
                    })
                    .expect("what json encode eroor"),
                )
                .header("Content-Type", "application/json")
                .send()
                .await
                .expect("Request Failed")
                .json()
                .await
                .expect("Response is ALL WRONG");
            let mut rejected = false;
            let mut rejected_attrs = vec![];
            let mut passed_attrs = vec![];
            let mut sum = 0f64;
            for (attribute, score) in response.attributeScores.iter() {
                let threshold = self
                    .config
                    .attributes
                    .get(attribute)
                    .expect("Response doesn't have the score I ASKED")
                    .threshold;
                let score = score.summaryScore.value;
                if score > threshold {
                    rejected = true;
                }
                let parsed = AttributeParsed {
                    score,
                    rejected: score > threshold,
                    name: attribute.clone(),
                    emoji: if score > threshold {
                        self.config.emotes.failed_check.clone()
                    } else {
                        self.config.emotes.passed_check.clone()
                    },
                };
                if score > threshold {
                    rejected_attrs.push(parsed)
                } else {
                    passed_attrs.push(parsed)
                }
                sum += score;
            }
            rejected_attrs.sort_unstable();
            rejected_attrs.reverse();
            passed_attrs.sort_unstable();
            passed_attrs.reverse();
            if rejected_attrs.len() > 1 {
                rejected_attrs.first_mut().unwrap().emoji =
                    self.config.emotes.failed_check_highest.clone();
                rejected_attrs.last_mut().unwrap().emoji =
                    self.config.emotes.failed_check_lowest.clone();
            }
            if passed_attrs.len() > 1 {
                passed_attrs.first_mut().unwrap().emoji =
                    self.config.emotes.passed_check_highest.clone();
                passed_attrs.last_mut().unwrap().emoji =
                    self.config.emotes.passed_check_lowest.clone();
            }
            let max = f64::max(
                passed_attrs.first().map(|e| e.score).unwrap_or(-INFINITY),
                rejected_attrs.first().map(|e| e.score).unwrap_or(-INFINITY),
            );
            let min = f64::min(
                passed_attrs.last().map(|e| e.score).unwrap_or(INFINITY),
                rejected_attrs.last().map(|e| e.score).unwrap_or(INFINITY),
            );
            rejected_attrs.append(&mut passed_attrs);
            if rejected {
                ctx.http
                    .get_channel(u64::from(self.logging_channel))
                    .await
                    .expect("Logging channel is not real")
                    .guild()
                    .expect("Logging channel is not in a guild")
                    .send_message(&ctx, |m| {
                        m.embed(|e| {
                            e.title("Message rejected")
                                .author(|a| {
                                    a.icon_url(
                                        msg.author
                                            .avatar_url()
                                            .unwrap_or_else(|| msg.author.default_avatar_url()),
                                    )
                                    .name(format!(
                                        "{}#{:04}",
                                        msg.author.name, msg.author.discriminator
                                    ))
                                })
                                .field("Message", msg.content.clone(), false)
                                .field(
                                    "Checks",
                                    rejected_attrs
                                        .iter()
                                        .map(|e| e.to_string())
                                        .collect::<String>(),
                                    true,
                                )
                                .field(
                                    "Stats",
                                    format!(
                                        "Min: {:.1}%\nMax: {:.1}%\nAvg: {:.1}%",
                                        min * 100f64,
                                        max * 100f64,
                                        (sum / response.attributeScores.len() as f64) * 100f64
                                    ),
                                    true,
                                )
                                .color(0xFF746D)
                        })
                    })
                    .await
                    .expect("Failed to log message");
                msg.delete(&ctx).await.expect("Failed to delete message");
            }
        }
    }

    // Set a handler to be called on the `ready` event. This is called when a
    // shard is booted, and a READY payload is sent by Discord. This payload
    // contains data like the current user's guild Ids, current user data,
    // private channels, and more.
    //
    // In this case, just print what the current user's username is.
    async fn ready(&self, _: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

#[tokio::main]
async fn main() {
    // Configure the client with tokens in the environment.
    let discord_token =
        env::var("DISCORD_TOKEN").expect("Expected DISCORD_TOKEN in the environment");

    let perspective_token =
        env::var("PERSPECTIVE_TOKEN").expect("Expected PERSPECTIVE_TOKEN in the environment");

    let sensored_channel = env::var("SENSORED_CHANNEL")
        .expect("Expected SENSORED_CHANNEL in the environment")
        .parse()
        .expect("Expected valid sensored channel id in the environment");

    let logging_channel = env::var("LOGGING_CHANNEL")
        .expect("Expected LOGGING_CHANNEL in the environment")
        .parse()
        .expect("Expected valid logging channel id in the environment");
    let mut config = String::new();
    File::open("config.json")
        .await
        .expect("Expected config.json")
        .read_to_string(&mut config)
        .await
        .expect("config.json read failed");
    let config: Config = serde_json::from_str(&*config).expect("Expected valid config.json");

    // Create a new instance of the Client, logging in as a bot. This will
    // automatically prepend your bot token with "Bot ", which is a requirement
    // by Discord for bot users.
    let mut client = Client::builder(&discord_token)
        .event_handler(Handler {
            perspective_token,
            sensored_channel,
            logging_channel,
            http_client: reqwest::Client::new(),
            config,
        })
        .await
        .expect("Err creating client");

    // Finally, start a single shard, and start listening to events.
    //
    // Shards will automatically attempt to reconnect, and will perform
    // exponential backoff until it reconnects.
    if let Err(why) = client.start().await {
        println!("Client error: {:?}", why);
    }
}
