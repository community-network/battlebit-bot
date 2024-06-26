use ab_glyph::{FontRef, PxScale};
use anyhow::Result;
use chrono::Utc;
use image::{io::Reader as ImageReader, Rgba};
use imageproc::drawing::draw_text_mut;
use serde::{Deserialize, Serialize};
use serenity::{
    builder::{CreateAttachment, EditProfile},
    client::{Client, Context, EventHandler},
    gateway::ActivityData,
    model::gateway::Ready,
    prelude::GatewayIntents,
};
use std::{collections::HashMap, env, io::Cursor};
use std::{
    sync::{atomic, Arc},
    time,
};
use warp::Filter;

struct Handler;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Static {
    pub token: String,
    pub server_name: String,
    pub set_banner_image: bool,
}

/// `MyConfig` implements `Default`
impl ::std::default::Default for Static {
    fn default() -> Self {
        Self {
            token: "".into(),
            server_name: "".into(),
            set_banner_image: true,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BattlebitServer {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Map")]
    pub map: String,
    #[serde(rename = "MapSize")]
    pub map_size: String,
    #[serde(rename = "Gamemode")]
    pub gamemode: String,
    #[serde(rename = "Region")]
    pub region: String,
    #[serde(rename = "Players")]
    pub players: i64,
    #[serde(rename = "QueuePlayers")]
    pub queue_players: i64,
    #[serde(rename = "MaxPlayers")]
    pub max_players: i64,
    #[serde(rename = "Hz")]
    pub hz: i64,
    #[serde(rename = "DayNight")]
    pub day_night: String,
    #[serde(rename = "IsOfficial")]
    pub is_official: bool,
    #[serde(rename = "HasPassword")]
    pub has_password: bool,
    #[serde(rename = "AntiCheat")]
    pub anti_cheat: String,
    #[serde(rename = "Build")]
    pub build: String,
}

#[serenity::async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, _: Ready) {
        let user = ctx.cache.current_user().clone();
        log::info!("Logged in as {:#?}", user.name);

        let last_update = Arc::new(atomic::AtomicI64::new(0));
        let last_update_clone = Arc::clone(&last_update);

        let cfg: Static = confy::load_path("config.txt").unwrap_or_default();
        log::info!("Started monitoring server {}", cfg.server_name);

        tokio::spawn(async move {
            let hello = warp::any().map(move || {
                let last_update_i64 = last_update_clone.load(atomic::Ordering::Relaxed);
                let now_minutes = Utc::now().timestamp() / 60;
                if (now_minutes - last_update_i64) > 5 {
                    warp::reply::with_status(
                        format!("{}", now_minutes - last_update_i64),
                        warp::http::StatusCode::SERVICE_UNAVAILABLE,
                    )
                } else {
                    warp::reply::with_status(
                        format!("{}", now_minutes - last_update_i64),
                        warp::http::StatusCode::OK,
                    )
                }
            });
            warp::serve(hello).run(([0, 0, 0, 0], 3030)).await;
        });

        // loop in seperate async
        tokio::spawn(async move {
            loop {
                match status(ctx.clone(), cfg.clone()).await {
                    Ok(item) => item,
                    Err(e) => {
                        log::error!("cant get new stats: {}", e);
                    }
                };
                last_update.store(Utc::now().timestamp() / 60, atomic::Ordering::Relaxed);
                // wait 2 minutes before redo
                tokio::time::sleep(time::Duration::from_secs(60)).await;
            }
        });
    }
}

async fn get() -> Result<Vec<BattlebitServer>> {
    let client = reqwest::Client::new();
    let url = "https://publicapi.battlebit.cloud/Servers/GetServerList";

    match client.get(url).send().await {
        Ok(resp) => {
            let mut json_string = resp.text().await.unwrap_or_default();
            // remove weird 0 width character
            // https://github.com/seanmonstar/reqwest/issues/426
            let json_bytes = json_string.as_bytes();
            if json_bytes[0] == 239 {
                json_string.remove(0);
            }
            match serde_json::from_str::<Vec<BattlebitServer>>(&json_string) {
                Ok(json_res) => Ok(json_res),
                Err(e) => {
                    anyhow::bail!("BattleBit public json is incorrect: {:#?}", e)
                }
            }
        }
        Err(e) => {
            anyhow::bail!("Battlebit public url failed: {:#?}", e)
        }
    }
}

async fn status(ctx: Context, statics: Static) -> Result<()> {
    match get().await {
        Ok(status) => {
            for server in status {
                if server.name == statics.server_name {
                    let server_info = format!(
                        "{}/{} - {}",
                        server.players,
                        server.max_players,
                        server.map.replace("Old_", "")
                    );
                    // change game activity
                    ctx.set_activity(Some(ActivityData::playing(server_info)));

                    let image_loc = gen_img(server).await?;

                    // change avatar
                    let avatar = CreateAttachment::path(image_loc)
                        .await
                        .expect("Failed to read image");
                    let mut user = ctx.cache.current_user().clone();
                    let mut new_profile = EditProfile::new().avatar(&avatar);
                    if statics.set_banner_image {
                        let banner = CreateAttachment::path("./info_image.jpg")
                            .await
                            .expect("Failed to read banner image");
                        new_profile = new_profile.banner(&banner);
                    }
                    let _ = user.edit(ctx, new_profile.clone()).await;

                    return Ok(());
                }
            }
        }
        Err(e) => {
            let server_info = "¯\\_(ツ)_/¯ server not found";
            ctx.set_activity(Some(ActivityData::playing(server_info)));

            anyhow::bail!(format!("Failed to get new serverinfo: {}", e))
        }
    };
    anyhow::bail!(format!("Couldn't find server in serverlist!"))
}

pub async fn gen_img(server: BattlebitServer) -> Result<String> {
    let client = reqwest::Client::new();
    let img = client
        .get(format!(
            "https://cdn.gametools.network/maps/battlebit/{}.jpg",
            server.map.replace("Old_", "")
        ))
        .send()
        .await?
        .bytes()
        .await?;

    let mut img2 = ImageReader::new(Cursor::new(img))
        .with_guessed_format()?
        .decode()?;

    img2.save("./info_image.jpg")?;
    img2.brighten(-25);

    let scale = PxScale {
        x: (img2.width() / 3) as f32,
        y: (img2.height() as f32 / 1.7),
    };
    let font = FontRef::try_from_slice(include_bytes!("Futura.ttf") as &[u8]).unwrap();

    let img_size = PxScale {
        x: img2.width() as f32,
        y: img2.height() as f32,
    };

    let small_modes = HashMap::from([
        ("CONQ", "CQ"),
        ("FRONTLINE", "FL"),
        ("RUSH", "RS"),
        ("DOMI", "DM"),
        ("TDM", "TDM"),
        ("INFCONQ", "IQ"),
        ("GunGameFFA", "GGF"),
        ("FFA", "FFA"),
        ("GunGameTeam)", "GGT"),
        ("ELI", "ELI"),
    ]);

    let small_mode = small_modes
        .get(&server.gamemode[..])
        .unwrap_or(&"")
        .to_string();

    draw_text_mut(
        &mut img2,
        Rgba([255u8, 255u8, 255u8, 255u8]),
        (img_size.x / 3.5) as i32,
        (img_size.y / 6.0) as i32,
        scale,
        &font,
        &small_mode[..],
    );
    img2.save("./map_mode.jpg")?;

    Ok(String::from("./map_mode.jpg"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    log::set_max_level(log::LevelFilter::Info);
    flexi_logger::Logger::try_with_str("warn,discord_bot=info")
        .unwrap_or_else(|e| panic!("Logger initialization failed with {}", e))
        .start()?;

    let mut cfg: Static = match confy::load_path("config.txt") {
        Ok(config) => config,
        Err(e) => {
            log::error!("error in config.txt: {}", e);
            log::warn!("changing back to default..");
            Static::default()
        }
    };
    cfg.token = match env::var("token") {
        Ok(res) => res,
        Err(_) => cfg.token,
    };
    cfg.server_name = match env::var("server_name") {
        Ok(res) => res,
        Err(_) => cfg.server_name,
    };
    cfg.set_banner_image = match env::var("set_banner_image") {
        Ok(res) => match res.as_str() {
            "true" => true,
            "t" => true,
            "false" => false,
            "f" => false,
            _ => true,
        },
        Err(_) => cfg.set_banner_image,
    };
    confy::store_path("config.txt", cfg.clone()).unwrap();

    // Login with a bot token from the environment
    let intents = GatewayIntents::non_privileged();
    let mut client = Client::builder(cfg.token, intents)
        .event_handler(Handler)
        .await
        .expect("Error creating client");

    // start listening for events by starting a single shard
    if let Err(why) = client.start().await {
        log::error!("Client error: {:?}", why);
    }
    Ok(())
}
