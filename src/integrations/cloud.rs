//! Cloud WebSocket transport. The Cloud protocol uses snapshot/update frames, not local JSON-RPC.
//! TODO: PIN registration HTTP actions, cloud-invoked API method dispatcher, full RemoteDashboard snapshot.
use super::{Worker, read_integrations, publish_connection};
use std::{io, time::Duration, collections::HashMap};
use tokio::sync::oneshot;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub struct CloudService { worker: Worker }
pub fn init(config: &crate::config::Config) -> io::Result<CloudService> {
    let cloud_url = config.cloud.url.clone();
    Ok(CloudService {worker: Worker::start("cloud", move |stop| run(cloud_url,stop))?})
}
impl CloudService {pub async fn shutdown(&mut self) -> io::Result<()> {self.worker.shutdown().await} }
fn cloud_ws_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if let Some(v)=base.strip_prefix("https://") {format!("wss://{v}/ws/streambot")}
    else if let Some(v)=base.strip_prefix("http://") {format!("ws://{v}/ws/streambot")}
    else {base.to_string()}
}
fn allowed(method: &str) -> bool {
    matches!(method, "notify_music_update"|"notify_playlist_update"|"notify_giveaway_update"|
    "notify_interaction"|"notify_interaction_queue"|"notify_auto_macros_update"|"notify_macro_update"|
    "notify_channel_point_update"|"notify_rotating_scene_update"|"notify_rotating_scene_runtime_update"|
    "notify_audio_update"|"notify_audio_outputs_update"|"notify_audio_presets_update"|
    "notify_obs_scene_update"|"notify_obs_scene_update_named"|"notify_obs_audio_update"|
    "notify_obs_audio_update_named"|"notify_yolobox_update"|"notify_connection_update"|
    "notify_song_request_update"|"notify_twitch_user_list")
}
async fn run(base: String, mut stop: oneshot::Receiver<()>) {
    let mut failed = 0usize;
    loop {
        let cfg = read_integrations(); let cloud = &cfg["cloud"];
        let enabled = cloud["enabled"].as_bool().unwrap_or(false);
        let token = cloud["instance_token"].as_str().unwrap_or("");
        if !enabled || token.is_empty() {
            publish_connection("cloud",enabled,if enabled {"auth_required"} else {"disconnected"},false);
            tokio::select! {_=&mut stop=>break,_=tokio::time::sleep(Duration::from_secs(5))=>continue}
        }
        let base_ws = cloud["websocket_url"].as_str().filter(|s|!s.is_empty()).map(str::to_string).unwrap_or_else(||cloud_ws_url(&base));
        let separator = if base_ws.contains('?') {'&'} else {'?'};
        let encoded: String = url::form_urlencoded::byte_serialize(token.as_bytes()).collect();
        let target=format!("{base_ws}{separator}token={encoded}");
        publish_connection("cloud",true,"connecting",false);
        let connect = tokio::select! {_=&mut stop=>break, result=connect_async(&target)=>result};
        match connect {
            Ok((ws,_)) => {
                failed=0; publish_connection("cloud",true,"connected",true);
                let (mut sink, mut source)=ws.split();
                let snapshots: HashMap<String,Value> = crate::server::registry::get().list().into_iter()
                    .filter(|method|allowed(method)).filter_map(|m|crate::server::registry::get().snapshot(&m).map(|v|(m,v))).collect();
                // TODO: replace partial snapshot with full RemoteDashboard fields when migrated.
                let snapshot=json!({"type":"snapshot","data":{"status":snapshots,"dashboard":{}}});
                if sink.send(Message::Text(snapshot.to_string().into())).await.is_err(){continue}
                let mut updates=crate::server::registry::get().subscribe();
                let mut baseline=snapshots;
                loop {
                    tokio::select! {
                        _=&mut stop => {let _=sink.close().await; return;},
                        update=updates.recv()=> {
                            let Ok(update)=update else {continue};
                            if !allowed(&update.method) {continue}
                            let (payload,partial)=match baseline.get(&update.method) {
                                Some(old)=>match crate::server::registry::delta(old,&update.data) {Some(v)=>(v,true),None=>continue},
                                None=>(update.data.clone(),false)
                            };
                            let packet=json!({"type":"update","method":update.method,"data":payload,"partial":partial});
                            if sink.send(Message::Text(packet.to_string().into())).await.is_err(){break}
                            baseline.insert(update.method,update.data);
                        },
                        msg=source.next()=>match msg {
                            Some(Ok(Message::Ping(p))) => {if sink.send(Message::Pong(p)).await.is_err(){break}},
                            Some(Ok(Message::Text(_))) => { /* TODO: authenticated Cloud API dispatch */ },
                            Some(Ok(Message::Close(_)))|None|Some(Err(_))=>break,
                            _=>{}
                        }
                    }
                }
            },
            Err(err)=>crate::internal::log::warn(&format!("Cloud WebSocket: {err}"))
        }
        publish_connection("cloud",true,"disconnected",false);
        failed+=1;
        if failed>=5 { crate::internal::log::warn("Cloud reconnect attempts exhausted");break }
        tokio::select! {_=&mut stop=>break,_=tokio::time::sleep(Duration::from_secs(5))=>{}}
    }
    publish_connection("cloud",false,"disconnected",false);
}
