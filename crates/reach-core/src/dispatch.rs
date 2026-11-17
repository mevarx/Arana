//! Ordered fallback, cross-process backend health, channel cooldown and pacing.
use crate::{
    clock::Clock,
    envelope,
    state::{FileLock, Store},
    ReachError, Result,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    any::Any,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub const SEARCH_INTERVAL: f64 = 10.0;
pub const RATE_LIMIT_SECONDS: f64 = 3600.0;
pub const PUBLIC_MAX_SECONDS: f64 = 20.0;
pub fn epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn stamp_age(value: &Value) -> f64 {
    let then = value.as_f64().or_else(|| {
        value
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp() as f64)
    });
    then.map(|t| (epoch() - t).max(0.0))
        .unwrap_or(f64::INFINITY)
}
fn ttl(code: &str) -> f64 {
    match code {
        "TIMEOUT" | "NEED_SETUP" => 60.0,
        "UPSTREAM_BROKEN" => 120.0,
        _ => 0.0,
    }
}
pub struct PublicRoute {
    pub action: String,
    pub params: Value,
    pub scopes: Vec<String>,
}
pub struct Request {
    pub channel: String,
    pub action: String,
    pub backends: Vec<String>,
    pub params: Value,
    pub timeout: Duration,
    pub only_backend: Option<String>,
    pub public: Option<PublicRoute>,
    pub added: bool,
    pub paced: bool,
    pub fingerprint: String,
}
#[async_trait]
pub trait Backend: Sync {
    async fn call(
        &self,
        channel: &str,
        action: &str,
        backend: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Vec<Value>>;
    async fn slot(&self, _channel: &str, _clock: &Clock) -> Result<Option<Box<dyn Any + Send>>> {
        Ok(None)
    }
}
async fn deadline<F, T>(future: F, clock: &Clock) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    tokio::pin!(future);
    loop {
        let left = clock.remaining();
        if left.is_zero() && !clock.paused() {
            return Err(ReachError::new(
                "TIMEOUT",
                "upstream did not answer in time",
            ));
        }
        tokio::select! {out=&mut future=>return out,_=tokio::time::sleep(left.min(Duration::from_millis(100)).max(Duration::from_millis(1)))=>{}}
    }
}
fn paused(store: &Store, r: &Request, b: &str) -> Result<Option<(ReachError, u64)>> {
    let v = store.load()?;
    let row = &v["backend_health"][&r.channel][&r.action][b];
    if row["fingerprint"].as_str() != Some(&r.fingerprint) {
        return Ok(None);
    }
    let code = row["code"].as_str().unwrap_or("");
    let left = ttl(code) - stamp_age(&row["at"]);
    Ok(if left > 0.0 {
        Some((
            ReachError::new(
                code,
                format!("{b} is temporarily paused after {code}; retry later"),
            ),
            left as u64 + 1,
        ))
    } else {
        None
    })
}
fn cooldown(store: &Store, channel: &str) -> Result<Option<ReachError>> {
    let v = store.load()?;
    for k in ["cooldowns", "setup_failures"] {
        let row = &v[k][channel];
        let left = RATE_LIMIT_SECONDS - stamp_age(&row["at"]);
        if row["code"] == "RATE_LIMITED" && left > 0.0 {
            let mut e = ReachError::new(
                "RATE_LIMITED",
                row["message"].as_str().unwrap_or("Rate limited"),
            );
            e.hint = format!(
                "Wait {} seconds before retrying this channel.",
                left as u64 + 1
            );
            return Ok(Some(e));
        }
    }
    Ok(None)
}
fn remember(store: &Store, r: &Request, b: &str, error: Option<&ReachError>) -> Result<()> {
    store.mutate(|v|{
    let channel=&mut v["backend_health"];
    if !channel[&r.channel].is_object(){channel[&r.channel]=json!({});}
    if !channel[&r.channel][&r.action].is_object(){channel[&r.channel][&r.action]=json!({});}
    let health=&mut channel[&r.channel][&r.action];
    if let Some(e)=error{if ttl(&e.code)>0.0{health[b]=json!({"code":e.code,"at":epoch(),"fingerprint":r.fingerprint});}}else{health.as_object_mut().unwrap().remove(b);}
    if error.is_some_and(|e|e.code=="RATE_LIMITED"){v["cooldowns"][&r.channel]=json!({"code":"RATE_LIMITED","message":"Platform reported a rate limit","at":epoch()});}
})
}
pub async fn pace(store: &Store, channel: &str, clock: &Clock) -> Result<()> {
    let _lock = FileLock::wait(&store.root, &format!("pace:{channel}"), clock.remaining()).await?;
    let age = stamp_age(&store.load()?["request_starts"][channel]);
    let wait = SEARCH_INTERVAL - age;
    if wait > 0.0 {
        if wait >= clock.remaining().as_secs_f64() {
            return Err(ReachError::new(
                "TIMEOUT",
                "Search pacing exceeds the remaining timeout; retry later",
            ));
        }
        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
    }
    store.mutate(|v| v["request_starts"][channel] = json!(epoch()))
}
fn best(mut errors: Vec<ReachError>) -> ReachError {
    let idx = errors
        .iter()
        .position(ReachError::is_final)
        .or_else(|| errors.iter().position(|e| e.code != "NEED_SETUP"))
        .unwrap_or(0);
    if errors.is_empty() {
        ReachError::new("BAD_INPUT", "No eligible backend")
    } else {
        errors.remove(idx)
    }
}
fn elapsed(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}
pub async fn run<B: Backend>(backend: &B, store: &Store, r: &Request) -> Value {
    let start = Instant::now();
    let clock = Clock::new(r.timeout);
    let mut attempts = vec![];
    let mut errors = vec![];
    let mut cached = vec![];
    let outcome:Result<Option<Value>>=async{
        if let Some(e)=cooldown(store,&r.channel)?{return Err(e);}
        let public_only=r.only_backend.as_deref()==Some("public-web");
        let public_first=r.public.is_some()&&r.only_backend.is_none()&&!r.added;
        if public_only||public_first{
            let route=r.public.as_ref().ok_or_else(||ReachError::new("BAD_INPUT","This input has no public web route; use a full supported URL"))?;
            let budget=if public_only{clock.remaining()}else{Duration::from_secs_f64(PUBLIC_MAX_SECONDS.min(clock.remaining().as_secs_f64()/2.0))};
            let out=public_attempt(backend,store,r,route,budget).await;
            attempts.extend(out["meta"]["attempts"].as_array().cloned().unwrap_or_default());
            if out["ok"]==true||public_only{return Ok(Some(out));}
        }
        let selected:Vec<_>=r.backends.iter().filter(|b|r.only_backend.as_ref().is_none_or(|only|only==*b)).collect();
        if selected.is_empty(){return Err(ReachError::new("BAD_INPUT",format!("no backend named '{}'",r.only_backend.as_deref().unwrap_or(""))));}
        let _permit=backend.slot(&r.channel,&clock).await?;
        if let Some(e)=cooldown(store,&r.channel)?{return Err(e);}
        let mut paced=false;
        for b in selected{
            if r.only_backend.is_none(){if let Some((e,left))=paused(store,r,b)?{attempts.push(json!({"backend":b,"result":e.code,"skipped":true,"retry_after_seconds":left,"message":e.message}));cached.push(e);continue;}}
            if clock.remaining().is_zero(){let e=ReachError::new("TIMEOUT","no time left for this backend");attempts.push(json!({"backend":b,"result":e.code}));errors.push(e);break;}
            if r.paced&&!paced&&matches!(r.action.as_str(),"search"|"jobs"){pace(store,&r.channel,&clock).await?;paced=true;}
            match deadline(backend.call(&r.channel,&r.action,b,&r.params,clock.remaining()),&clock).await{
                Ok(items)=>{attempts.push(json!({"backend":b,"result":"ok"}));remember(store,r,b,None)?;return Ok(Some(envelope(Some(&r.channel),Some(&r.action),Some(b),items,elapsed(start),attempts.clone(),None)));}
                Err(e)=>{attempts.push(json!({"backend":b,"result":e.code,"message":e.message}));remember(store,r,b,Some(&e))?;let stop=e.is_final();errors.push(e);if stop{break;}}
            }
        }
        let failures:Vec<_>=errors.iter().chain(cached.iter()).collect();
        if let Some(route)=r.public.as_ref(){
        if r.only_backend.is_none()&&!public_first&&!failures.is_empty()&&!failures.iter().any(|e|e.is_final())&&failures.iter().any(|e|matches!(e.code.as_str(),"UPSTREAM_BROKEN"|"TIMEOUT"))&&!clock.remaining().is_zero(){
            let mut out=public_attempt(backend,store,r,route,clock.remaining()).await;
            attempts.extend(out["meta"]["attempts"].as_array().cloned().unwrap_or_default());
            if out["ok"]==true{out["meta"]["attempts"]=json!(attempts);return Ok(Some(out));}
        }}
        Ok(None)
    }.await;
    let mut out = match outcome {
        Ok(Some(v)) => v,
        Ok(None) => {
            if errors.is_empty() {
                errors = cached;
            }
            envelope(
                Some(&r.channel),
                Some(&r.action),
                None,
                vec![],
                elapsed(start),
                attempts,
                Some(best(errors)),
            )
        }
        Err(e) => envelope(
            Some(&r.channel),
            Some(&r.action),
            None,
            vec![],
            elapsed(start),
            attempts,
            Some(e),
        ),
    };
    out["meta"]["elapsed_ms"] = json!(elapsed(start));
    out
}
fn in_scope(target: &str, scopes: &[String]) -> bool {
    let Ok(url) = url::Url::parse(target) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let host = url.host_str().unwrap_or("");
    scopes.iter().any(|scope| {
        let (site, path) = scope.split_once('/').unwrap_or((scope, ""));
        (host == site || host.ends_with(&format!(".{site}")))
            && (path.is_empty() || url.path().starts_with(&format!("/{path}")))
    })
}
fn readable(item: &Value) -> bool {
    let text = item["text"].as_str().unwrap_or("").trim();
    let title = item["title"].as_str().unwrap_or("").trim();
    if text.is_empty() || (text.chars().count() < 80 && text.eq_ignore_ascii_case(title)) {
        return false;
    }
    static WALL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^(?:log\s*in|sign\s*in|join\s+(?:now|linkedin)|登录|登入|注册|just a moment|prove your humanity|access denied|security verification|安全验证|验证码)(?:\s*[-|·:].*)?$").unwrap()
    });
    static ONLY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^(?:(?:please\s+)?(?:log\s*in|sign\s*in)(?:\s+to\s+continue)?[.!?\s]*|(?:请)?(?:登录|登入)(?:后继续|以继续)?[。！\s]*)+$").unwrap()
    });
    !WALL.is_match(title) && !ONLY.is_match(text)
}
async fn public_attempt<B: Backend>(
    b: &B,
    store: &Store,
    r: &Request,
    route: &PublicRoute,
    budget: Duration,
) -> Value {
    let web = Request {
        channel: "web".into(),
        action: route.action.clone(),
        backends: if route.action == "read" {
            vec!["obscura".into(), "jina".into(), "exa".into()]
        } else {
            vec!["exa".into()]
        },
        params: route.params.clone(),
        timeout: budget,
        only_backend: None,
        public: None,
        added: true,
        paced: false,
        fingerprint: r.fingerprint.clone(),
    };
    // The boxed recursive call is bounded: the web request never has a public route.
    let mut out = Box::pin(run(b, store, &web)).await;
    out["channel"] = json!(r.channel);
    out["action"] = json!(r.action);
    if out["ok"] == true {
        let scope = if route.action == "search" {
            "search_results"
        } else {
            "page_text"
        };
        let items:Vec<_>=out["items"].as_array().unwrap().iter().filter(|i|in_scope(i["url"].as_str().unwrap_or(""),&route.scopes)&&(route.action!="read"||readable(i))).map(|i|{let mut item=i.clone();item["raw"]=json!({"source":item["raw"],"content_scope":scope,"access":"public","completeness_verified":false});item}).collect();
        if items.is_empty() {
            out["ok"] = json!(false);
            out["backend"] = Value::Null;
            out["items"] = json!([]);
            let mut e = ReachError::new(
                "UPSTREAM_BROKEN",
                "Public web returned no usable content in the requested scope",
            );
            e.hint = "Try the site tool; this does not establish a login requirement.".into();
            out["error"] = json!(e);
            out["meta"]["attempts"]
                .as_array_mut()
                .unwrap()
                .push(json!({"backend":"content-check","result":"INSUFFICIENT_CONTENT"}));
        } else {
            out["items"] = json!(items);
            out["backend"] = json!("public-web");
            out["meta"]["access"] = json!({"mode":"public","scope":scope,"completeness_verified":false,"note":"公开网页结果；不保证站内排序、最新状态、全文或全部评论。需要站内数据时指定 --backend。"});
        }
    }
    for row in out["meta"]["attempts"].as_array_mut().unwrap() {
        row["backend"] = json!(format!(
            "public-web/{}",
            row["backend"].as_str().unwrap_or("")
        ));
    }
    out
}
