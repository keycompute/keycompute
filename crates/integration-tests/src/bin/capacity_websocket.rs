//! Bounded native WSS workload for a disposable, local capacity lab.
//! Uses the library WebSocket parser and a trusted lab CA; never disables TLS.
use anyhow::{Context, ensure};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{collections::HashMap, io::Write, sync::Arc, time::Duration};
use tokio::time::{Instant, sleep_until, timeout};
use tokio_rustls::{
    TlsConnector,
    rustls::{
        self, RootCertStore,
        pki_types::{CertificateDer, ServerName, pem::PemObject},
    },
};
use tokio_tungstenite::{
    client_async_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

struct Pending {
    start: Instant,
    lag: f64,
    first: Option<f64>,
    content: bool,
    bytes: usize,
}
fn elapsed(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
fn number(v: &Value, key: &str, lo: u64, hi: u64) -> anyhow::Result<u64> {
    let n = v[key].as_u64().context("missing numeric setting")?;
    ensure!((lo..=hi).contains(&n), "invalid {key}");
    Ok(n)
}
fn percentiles(mut values: Vec<f64>) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    values.sort_by(f64::total_cmp);
    let p = |q: f64| values[((values.len() - 1) as f64 * q) as usize];
    json!({"p50":p(0.5),"p95":p(0.95),"p99":p(0.99),"max":values[values.len()-1]})
}
fn terminal_ok(v: &Value) -> bool {
    v["status"] == "completed"
        && v["error"].is_null()
        && v["usage"]["total_tokens"] == 16
        && v["output"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["content"].as_array().is_some_and(|parts| {
                    parts.iter().any(|p| {
                        p["type"] == "output_text"
                            && p["text"].as_str().is_some_and(|s| !s.is_empty())
                    })
                })
            })
        })
}
async fn diagnostics(
    client: &reqwest::Client,
    fixture: &Value,
    replicas: u64,
) -> anyhow::Result<Value> {
    let token = fixture["admin_token"]
        .as_str()
        .context("missing admin token")?;
    let mut out = Vec::new();
    for i in 0..replicas {
        out.push(
            client
                .get(format!(
                    "http://gw{i}:3000/api/v1/admin/monitoring/capacity"
                ))
                .bearer_auth(token)
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await?,
        );
    }
    Ok(Value::Array(out))
}
#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    ensure!(
        std::env::var("KC_LAB_ACK").as_deref() == Ok("1"),
        "explicit lab acknowledgement required"
    );
    let fixture: Value = serde_json::from_slice(&std::fs::read("/lab/private/fixture.json")?)?;
    let settings: Value = serde_json::from_slice(&std::fs::read("/lab/private/settings.json")?)?;
    ensure!(
        fixture["run"].as_str() == Some(std::env::var("KC_LAB_RUN")?.as_str()),
        "fixture run mismatch"
    );
    ensure!(settings["protocol"] == "websocket", "WSS profile required");
    let rate = number(&settings, "rate", 1, 1000)?;
    let seconds = number(&settings, "seconds", 1, 3600)?;
    let planned = rate.checked_mul(seconds).context("load size overflow")?;
    ensure!(planned <= 200000, "load budget exceeded");
    let replicas = number(&settings, "replicas", 1, 4)?;
    let payload = number(&settings, "payload_bytes", 0, 8 * 1024 * 1024)? as usize;
    let stream_ms = number(&settings, "stream_ms", 10, 60000)?;
    let max_pending = number(&settings, "client_workers", 1, 256)?.min(16) as usize;
    let key = fixture["keys"][0].as_str().context("missing request key")?;
    let pem = std::fs::read("/lab/private/cert.pem")?;
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from_pem_slice(&pem)?)?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect("nginx:443"),
    )
    .await??;
    let tls = timeout(
        Duration::from_secs(5),
        TlsConnector::from(Arc::new(tls)).connect(ServerName::try_from("nginx")?, tcp),
    )
    .await??;
    let mut request = "wss://nginx/v1/responses".into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {key}").parse()?);
    let config = WebSocketConfig::default()
        .max_message_size(Some(16 * 1024 * 1024))
        .max_frame_size(Some(16 * 1024 * 1024));
    let (mut ws, _) = timeout(
        Duration::from_secs(5),
        client_async_with_config(request, tls, Some(config)),
    )
    .await??;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?;
    let before = diagnostics(&client, &fixture, replicas).await?;
    let start = Instant::now();
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open("/lab/results/load-started")?;
    let deadline = start
        + Duration::from_secs(seconds)
        + Duration::from_millis(stream_ms)
        + Duration::from_secs(30);
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut samples = Vec::new();
    let mut sent = 0u64;
    let mut drops = 0u64;
    let mut transport_failed = false;
    while sent < planned || !pending.is_empty() {
        let scheduled = start + Duration::from_secs_f64(sent as f64 / rate as f64);
        tokio::select! {
            _=sleep_until(deadline)=>{transport_failed=true;break;},
            _=sleep_until(scheduled),if sent<planned=>{
                sent+=1;
                if pending.len()>=max_pending{drops+=1;continue;}
                let lane=(0..16).map(|i|format!("lab_{i}")).find(|name|!pending.contains_key(name)).context("bounded lane missing")?;
                let body=json!({"type":"response.create","stream_id":lane,"model":"gpt-4o","input":format!("Hello {}","x".repeat(payload)),"max_output_tokens":32,"store":false});
                let t=Instant::now();let lag=t.saturating_duration_since(scheduled).as_secs_f64()*1000.0;
                pending.insert(lane,Pending{start:t,lag,first:None,content:false,bytes:0});
                if !matches!(timeout(Duration::from_secs(5),ws.send(Message::Text(body.to_string().into()))).await,Ok(Ok(()))){transport_failed=true;break;}
            },
            incoming=ws.next()=>{
                match incoming {
                    Some(Ok(Message::Text(text)))=>{
                        let value:Value=match serde_json::from_str(&text){Ok(v)=>v,Err(_)=>{transport_failed=true;break;}};
                        let lane=value["stream_id"].as_str().unwrap_or("").to_string();
                        let Some(p)=pending.get_mut(&lane) else {transport_failed=true;break;};
                        p.bytes=p.bytes.saturating_add(text.len());
                        if p.bytes>32*1024*1024{transport_failed=true;break;}
                        let kind=value["type"].as_str().unwrap_or("");
                        if kind=="response.output_text.delta" && value["delta"].as_str().is_some_and(|s|!s.is_empty()){
                            p.content=true;if p.first.is_none(){p.first=Some(elapsed(p.start));}
                        }
                        if matches!(kind,"response.completed"|"response.failed"|"response.incomplete"|"error"){
                            let good=kind=="response.completed" && p.content && terminal_ok(&value["response"]);
                            samples.push(json!({"complete":good,"elapsed_ms":elapsed(p.start),"first_content_ms":p.first,"lag_ms":p.lag}));
                            pending.remove(&lane);
                        }
                    },
                    Some(Ok(Message::Ping(payload)))=>{if !matches!(timeout(Duration::from_secs(2),ws.send(Message::Pong(payload))).await,Ok(Ok(()))){transport_failed=true;break;}},
                    Some(Ok(Message::Pong(_)))=>{},
                    _=>{transport_failed=true;break;}
                }
            }
        }
    }
    for p in pending.into_values() {
        samples.push(json!({"complete":false,"elapsed_ms":elapsed(p.start),"first_content_ms":p.first,"lag_ms":p.lag}));
    }
    let _ = timeout(Duration::from_secs(1), ws.close(None)).await;
    drop(ws);
    sleep_until(start + Duration::from_secs(seconds)).await;
    let elapsed_seconds = start.elapsed().as_secs_f64();
    let good: Vec<_> = samples.iter().filter(|v| v["complete"] == true).collect();
    let after = diagnostics(&client, &fixture, replicas).await?;
    let model: Value = client
        .get("http://model:8080/stats")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let report = json!({"schema":"keycompute-separated-wss-v1","settings":settings,"planned":planned,"sent":samples.len(),"completed_success":good.len(),"generator_dropped":drops,"warmups":0,
        "elapsed_seconds":elapsed_seconds,"successful_rps_including_tail":good.len() as f64/elapsed_seconds,
        "complete_ms":percentiles(good.iter().filter_map(|s|s["elapsed_ms"].as_f64()).collect()),
        "first_content_ms":percentiles(good.iter().filter_map(|s|s["first_content_ms"].as_f64()).collect()),
        "schedule_lag_ms":percentiles(samples.iter().filter_map(|s|s["lag_ms"].as_f64()).collect()),
        "status_counts":{"complete":good.len(),"failed":samples.len()-good.len()},"before":before,"after":after,"model":model,"transport_failed":transport_failed,
        "scope":"one verified TLS WebSocket, <=16 correlated lanes, no retries; not a many-connection benchmark"});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open("/lab/results/client.json")?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    ensure!(
        !transport_failed && drops == 0 && good.len() as f64 / planned as f64 >= 0.99,
        "WSS workload failed; report retained"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn success_requires_terminal_usage_and_content() {
        let mut v = json!({"status":"completed","usage":{"total_tokens":16},"output":[{"content":[{"type":"output_text","text":"ok"}]}]});
        assert!(terminal_ok(&v));
        v["status"] = json!("incomplete");
        assert!(!terminal_ok(&v));
        v["status"] = json!("completed");
        v["usage"] = Value::Null;
        assert!(!terminal_ok(&v));
        assert!(!terminal_ok(&json!([])));
    }
}
