//! Category Library persistence and API, migrated from CategoryLibraryHelper.ts.
//! TODO: Twitch game sync, Steam metadata, media downloads, OBS filters, channel points,
//! category-transition macros, CSS/media application, website imports.
use serde_json::{json, Value, Map};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::{Mutex, OnceLock}};

struct Library { categories: BTreeMap<String, Value>, active: Option<String> }
static LIBRARY: OnceLock<Mutex<Library>> = OnceLock::new();
fn state() -> &'static Mutex<Library> { LIBRARY.get_or_init(|| Mutex::new(load())) }
fn root() -> PathBuf { crate::config::file::config_root().join("category_library") }
fn valid_id(value: &Value) -> Result<String,String> {
    let id = match value { Value::String(s) => s.clone(), Value::Number(n) => n.to_string(), _ => String::new() };
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) { return Err("category_id is invalid".into()); }
    Ok(id)
}
fn stamp() -> String { chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true) }
fn load() -> Library {
    let mut result = Library {categories: BTreeMap::new(), active: None};
    let dir = root();
    if let Err(e) = fs::create_dir_all(&dir) { crate::internal::log::warn(&format!("Category library directory: {e}")); return result; }
    // Preserve the TS migration from legacy categories.json to per-category JSON files.
    let legacy = dir.join("categories.json");
    if let Ok(raw) = fs::read_to_string(&legacy) {
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            if let Some(categories) = value.get("categories").and_then(Value::as_object) {
                let mut migrated = true;
                for (id, raw) in categories {
                    if valid_id(&json!(id)).is_err() { continue; }
                    let mut entry = raw.clone();
                    if let Some(object) = entry.as_object_mut() { object.insert("category_id".into(),json!(id)); }
                    if let Err(e) = write_entry(&entry) { migrated = false; crate::internal::log::warn(&format!("Category migration failed for {id}: {e}")); }
                }
                if migrated { let _ = fs::remove_file(&legacy); }
            }
        }
    }
    if let Ok(items) = fs::read_dir(&dir) {
        for file in items.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else { continue };
            if valid_id(&json!(id)).is_err() {continue}
            if let Ok(text) = fs::read_to_string(file.path()) {
                if let Ok(mut entry) = serde_json::from_str::<Value>(&text) {
                    if !entry.is_object() || entry.as_object().is_some_and(|v|v.is_empty()) {continue}
                    if let Some(obj) = entry.as_object_mut() {obj.insert("category_id".into(),json!(id));}
                    if entry.get("active").and_then(Value::as_bool)==Some(true) { result.active=Some(id.to_owned()); }
                    result.categories.insert(id.to_owned(),entry);
                }
            }
        }
    }
    result
}
fn write_entry(value: &Value) -> Result<(),String> {
    let id=valid_id(value.get("category_id").unwrap_or(&Value::Null))?;
    fs::create_dir_all(root()).map_err(|e|e.to_string())?;
    let dst=root().join(format!("{id}.json"));
    let tmp=root().join(format!("{id}.json.tmp"));
    let bytes=serde_json::to_vec_pretty(value).map_err(|e|e.to_string())?;
    fs::write(&tmp,bytes).map_err(|e|e.to_string())?;
    fs::rename(tmp,dst).map_err(|e|e.to_string())
}
fn settings() -> Value {
    let file=crate::config::file::config_root().join("streambot-settings.json");
    fs::read_to_string(file).ok().and_then(|s|serde_json::from_str::<Value>(&s).ok())
        .and_then(|v|v.get("category_library").cloned())
        .unwrap_or_else(||json!({"enabled":false,"auto_create":true,"fetch_cover":true,"fetch_steam_wallpaper":true,"auto_theme_color":true,"apply_theme_color":true,"apply_media":true,"apply_custom_css":true,"apply_obs_filters":true,"apply_channel_points":true,"media_slots":[{"name":"horizontal","target":"horizontal","orientation":"horizontal","autoplay":true,"loop":true,"muted":true},{"name":"vertical","target":"vertical","orientation":"vertical","autoplay":true,"loop":true,"muted":true}]}))
}
fn active<'a>(library: &'a Library) -> Option<&'a Value> { library.active.as_ref().and_then(|id|library.categories.get(id)) }
fn active_payload(lib:&Library)->Value {
    json!({"category":active(lib),"effective_media":null,"effective_obs_filters":{},"active_channel_points":{"allowed":[],"blocked":[]}})
}
fn full_payload(lib:&Library)->Value {
    let cfg=settings(); let item=active(lib);
    let css= if cfg.get("enabled")==Some(&Value::Bool(true)) && cfg.get("apply_custom_css")!=Some(&Value::Bool(false)) {item.and_then(|v|v.get("custom_css")).cloned().unwrap_or(json!(""))} else {json!("")};
    let mut categories:Vec<Value>=lib.categories.values().cloned().collect();
    categories.sort_by_key(|v|v.get("name").and_then(Value::as_str).unwrap_or("").to_lowercase());
    json!({"version":1,"enabled":cfg.get("enabled").and_then(Value::as_bool).unwrap_or(false),"settings":cfg,
      "steam":{"configured":false},"active_category_id":lib.active,"active_category":item,
      "effective_media":null,"effective_obs_filters":{},"active_channel_points":{"allowed":[],"blocked":[]},
      "active_style":{"category_id":item.and_then(|v|v.get("category_id")),"category_name":item.and_then(|v|v.get("name")),"css":css},
      "categories":categories})
}
fn publish(lib:&Library) {
    let full=full_payload(lib);
    crate::server::registry::notify("notify_category_library_update",full.clone());
    crate::server::registry::notify("notify_category_active",active_payload(lib));
    crate::server::registry::notify("notify_category_style_update",full["active_style"].clone());
}
pub fn init()->Result<(),String> {
    let lib=state().lock().map_err(|e|e.to_string())?;
    publish(&lib);
    Ok(())
}
fn normalize(input:&Value, prior:Option<&Value>, id:&str)->Value {
    let mut obj=prior.and_then(Value::as_object).cloned().unwrap_or_else(Map::new);
    if let Some(changes)=input.as_object() {for (key,value) in changes { obj.insert(key.clone(),value.clone()); }}
    obj.remove("categoryId");
    obj.insert("category_id".into(),json!(id));
    if !obj.get("name").and_then(Value::as_str).is_some_and(|s|!s.trim().is_empty()) {obj.insert("name".into(),json!(format!("Category {id}")));}
    for name in ["obs_filters"] {if !obj.get(name).is_some_and(Value::is_object) {obj.insert(name.into(),json!({}));}}
    for name in ["channel_points","blocked_channel_points","macros_on_active","macros_on_inactive","custom_media"] {if !obj.get(name).is_some_and(Value::is_array) {obj.insert(name.into(),json!([]));}}
    for name in ["use_as_media_fallback","active"] {if !obj.get(name).is_some_and(Value::is_boolean) {obj.insert(name.into(),json!(false));}}
    if !obj.contains_key("created_at") {obj.insert("created_at".into(),json!(stamp()));}
    obj.insert("updated_at".into(),json!(stamp()));
    Value::Object(obj)
}
pub fn dispatch(method:&str,params:Value)->Result<Value,String> {
    let input=params.as_object().cloned().unwrap_or_default();
    match method {
       "category_library_list" => {let lib=state().lock().map_err(|e|e.to_string())?;Ok(full_payload(&lib))}
       "category_library_get" => {let id=valid_id(input.get("category_id").or_else(||input.get("categoryId")).unwrap_or(&Value::Null))?;
           let lib=state().lock().map_err(|e|e.to_string())?;
           lib.categories.get(&id).cloned().map(|category|json!({"category":category})).ok_or_else(||"category not found".into())}
       "category_library_save" => {
           let value=input.get("category").cloned().unwrap_or(params);
           let id=valid_id(value.get("category_id").or_else(||value.get("categoryId")).unwrap_or(&Value::Null))?;
           let mut lib=state().lock().map_err(|e|e.to_string())?;
           let item=normalize(&value,lib.categories.get(&id),&id);
           // Atomic persistence before any mutation or notification.
           write_entry(&item)?;
           if item.get("use_as_media_fallback")==Some(&json!(true)) {
               let ids:Vec<String>=lib.categories.keys().filter(|k|*k!=&id).cloned().collect();
               for other in ids {if let Some(old)=lib.categories.get(&other) {if old["use_as_media_fallback"]==true {let mut next=old.clone();next["use_as_media_fallback"]=json!(false);write_entry(&next)?;lib.categories.insert(other,next);}}}
           }
           lib.categories.insert(id,item.clone());publish(&lib);
           Ok(json!({"category":item}))
       }
       "category_library_delete" => {
           let id=valid_id(input.get("category_id").or_else(||input.get("categoryId")).unwrap_or(&Value::Null))?;
           let mut lib=state().lock().map_err(|e|e.to_string())?;
           match fs::remove_file(root().join(format!("{id}.json"))) {Ok(_)=>(),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>(),Err(e)=>return Err(e.to_string())}
           lib.categories.remove(&id);
           if lib.active.as_deref()==Some(&id) {lib.active=None;}
           publish(&lib);Ok(json!({"category_id":id}))
       }
       "category_library_activate" => {
           let id=valid_id(input.get("category_id").or_else(||input.get("categoryId")).unwrap_or(&Value::Null))?;
           let mut lib=state().lock().map_err(|e|e.to_string())?;
           if !lib.categories.contains_key(&id) {return Err("category not found".into())}
           let ids:Vec<String>=lib.categories.keys().cloned().collect();
           for key in ids {let next_active=key==id;let prev=&lib.categories[&key];
               if prev.get("active").and_then(Value::as_bool)!=Some(next_active) {
                   let mut next=prev.clone();next["active"]=json!(next_active);next["updated_at"]=json!(stamp());write_entry(&next)?;lib.categories.insert(key,next);
               }}
           lib.active=Some(id.clone());publish(&lib);
           Ok(json!({"category":lib.categories[&id]}))
       }
       "category_library_refresh"=>Err("TODO: Twitch/Steam category metadata refresh not migrated".into()),
       "category_library_import_website"=>Err("TODO: website category import not migrated".into()),
       _=>Err("Method not found".into())
    }
}
pub fn is_method(method:&str)->bool {matches!(method,"category_library_list"|"category_library_get"|"category_library_save"|"category_library_delete"|"category_library_activate"|"category_library_refresh"|"category_library_import_website")}

#[cfg(test)] mod tests {use super::*;#[test] fn invalid_ids(){assert!(valid_id(&json!("../../x")).is_err());assert_eq!(valid_id(&json!(123)).unwrap(),"123");}}
