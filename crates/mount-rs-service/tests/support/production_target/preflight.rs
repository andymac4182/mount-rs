//! Read-only observations of an existing local fixture. Never starts a provider.
use super::command::Commands;
use mysql_async::prelude::Queryable;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, time::Duration};
#[derive(Default)]
pub struct Owner {
    pool: Option<mysql_async::Pool>,
}
impl Owner {
    pub async fn close(&mut self) -> Result<(), String> {
        if let Some(pool) = self.pool.take() {
            tokio::time::timeout(Duration::from_secs(10), pool.disconnect())
                .await
                .map_err(|_| "preflight SQL disconnect unproven after10s")?
                .map_err(|_| "preflight SQL disconnect failed")?;
        }
        Ok(())
    }
}
async fn docker(commands: &mut Commands, args: &[&str]) -> Result<String, String> {
    commands
        .capture("docker", args, None, Duration::from_secs(10))
        .await
}
fn local_endpoint(value: &str) -> Result<(), String> {
    if !value.starts_with("unix:///") || value.contains(['\n', '\r']) {
        return Err(
            "preflight refusal: Docker daemon must use an explicit local Unix socket".into(),
        );
    }
    Ok(())
}
fn addresses(values: &Value, field: &str) -> Result<BTreeSet<String>, String> {
    values
        .as_array()
        .ok_or("cluster observation array missing")?
        .iter()
        .map(|v| {
            v[field]
                .as_str()
                .map(str::to_owned)
                .ok_or("cluster address missing".into())
        })
        .collect()
}
fn validate_health(
    members: &Value,
    health: &Value,
    stores: &Value,
    pd: &BTreeSet<String>,
    tikv: &BTreeSet<String>,
) -> Result<(), String> {
    let actual_members: BTreeSet<_> = members["members"]
        .as_array()
        .ok_or("PD members missing")?
        .iter()
        .map(|m| {
            let urls = m["client_urls"]
                .as_array()
                .ok_or("PD client URLs missing")?;
            if urls.len() != 1 {
                return Err("ambiguous PD endpoint".to_string());
            }
            Ok(urls[0]
                .as_str()
                .ok_or("PD endpoint missing")?
                .trim_start_matches("http://")
                .to_string())
        })
        .collect::<Result<_, String>>()?;
    if &actual_members != pd || members["members"].as_array().map(Vec::len) != Some(3) {
        return Err("PD membership does not match inspected fixture".into());
    }
    let h = health.as_array().ok_or("PD health missing")?;
    if h.len() != 3 || h.iter().any(|v| v["health"] != true) {
        return Err("PD members unhealthy".into());
    }
    let health_endpoints: BTreeSet<_> = h
        .iter()
        .flat_map(|v| v["client_urls"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .map(|s| s.trim_start_matches("http://").to_owned())
        .collect();
    if &health_endpoints != pd {
        return Err("PD health identity mismatch".into());
    }
    let s = stores["stores"].as_array().ok_or("TiKV stores missing")?;
    if s.len() != 3 || s.iter().any(|v| v["store"]["state_name"] != "Up") {
        return Err("TiKV stores not all Up".into());
    }
    let actual: BTreeSet<_> = s
        .iter()
        .map(|v| {
            v["store"]["address"]
                .as_str()
                .map(str::to_owned)
                .ok_or("TiKV address missing")
        })
        .collect::<Result<_, _>>()?;
    if &actual != tikv {
        return Err("TiKV membership does not match inspected fixture".into());
    }
    Ok(())
}
pub async fn tidb(
    output: &Path,
    commands: &mut Commands,
    owner: &mut Owner,
) -> Result<Value, String> {
    let path = output.join("tidb-preflight.json");
    let mut receipt = json!({"observed_unix_ms":super::utc_ms(),"qualified":false,"complete":false,"scope":"live local Docker VM and inspected existing cluster; VM floor is not host RAM or sum of container caps; caps are observations only"});
    super::write_json(&path, &receipt)?;
    let result=async{
        let run=std::env::var("MOUNT_RS_TARGET_TIDB_RUN").map_err(|_|"preflight refusal: explicit TiDB run label required")?;
        if !run.starts_with("mount-rs-tidb-")||!run.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'){return Err("preflight refusal: invalid fixture identity".into());}
        let connection=std::env::var("MOUNT_RS_TIDB_URL").map_err(|_|"TiDB URL required")?;
        let url=url::Url::parse(&connection).map_err(|_|"TiDB URL invalid (redacted)")?;
        if !matches!(url.host_str(),Some("127.0.0.1")|Some("localhost")){return Err("preflight refusal: local SQL endpoint required".into());}
        let port=url.port().unwrap_or(4000);
        if let Ok(host)=std::env::var("DOCKER_HOST"){local_endpoint(&host)?;receipt["docker_host_override"]=json!(host);}
        let context=docker(commands,&["context","show"]).await?;
        let context=context.trim();
        let endpoint=docker(commands,&["context","inspect",context,"--format","{{.Endpoints.docker.Host}}"]).await?;
        local_endpoint(endpoint.trim())?;
        receipt["docker_context"]=json!({"name":context,"endpoint":endpoint.trim(),"effective_endpoint":std::env::var("DOCKER_HOST").ok().filter(|_|std::env::var_os("DOCKER_CONTEXT").is_none()).unwrap_or_else(||endpoint.trim().into())});
        receipt["endpoint"]=json!({"host":"127.0.0.1","port":port});receipt["run"]=json!(run);
        let raw=docker(commands,&["info","--format",r#"{"memory_bytes":{{.MemTotal}},"cpus":{{.NCPU}},"id":{{json .ID}},"name":{{json .Name}},"server_version":{{json .ServerVersion}}}"#]).await?;
        receipt["docker_info"]=serde_json::from_str(&raw).map_err(|_|"Docker info invalid")?;
        super::write_json(&path,&receipt)?;
        let filter=format!("label=mount-rs.tidb.run={run}");let ids=docker(commands,&["ps","-a","--filter",&filter,"--format","{{.ID}}"]).await?;
        let mut containers=Vec::new();let mut roles=[0usize;3];let mut endpoint_matches=false;
        let mut pd=BTreeSet::new();let mut tikv=BTreeSet::new();let mut sql=BTreeSet::new();let mut networks=BTreeSet::new();let mut pd_id=String::new();
        for id in ids.lines(){
            let raw=docker(commands,&["inspect","--format",r#"{"id":{{json .Id}},"image":{{json .Config.Image}},"image_id":{{json .Image}},"run":{{json (index .Config.Labels "mount-rs.tidb.run")}},"running":{{json .State.Running}},"started_at":{{json .State.StartedAt}},"memory_cap":{{.HostConfig.Memory}},"ports":{{json .NetworkSettings.Ports}},"networks":{{json .NetworkSettings.Networks}}}"#,id]).await?;
            let mut v:Value=serde_json::from_str(&raw).map_err(|_|"Docker inspect invalid")?;
            if v["run"]!=run||v["running"]!=true{return Err("preflight refusal: fixture ownership/readiness mismatch".into());}
            let nets=v["networks"].as_object().ok_or("container network unavailable")?;
            if nets.len()!=1{return Err("preflight refusal: ambiguous container networks".into());}
            let (network_name,network)=nets.iter().next().unwrap();let ip=network["IPAddress"].as_str().ok_or("container IP missing")?.to_string();
            if ip.parse::<std::net::Ipv4Addr>().is_err(){return Err("invalid fixture IP".into());}
            let network_id=network["NetworkID"].as_str().ok_or("network ID missing")?.to_string();networks.insert(network_id.clone());
            v["networks"]=json!({"name":network_name,"id":network_id,"ip":ip});
            let image=v["image"].as_str().ok_or("image identity missing")?;
            if image.starts_with("pingcap/pd:"){roles[0]+=1;pd.insert(format!("{ip}:2379"));pd_id=id.into();}else if image.starts_with("pingcap/tikv:"){roles[1]+=1;tikv.insert(format!("{ip}:20160"));}else if image.starts_with("pingcap/tidb:"){roles[2]+=1;sql.insert(format!("{ip}:4000"));endpoint_matches=v["ports"]["4000/tcp"].as_array().is_some_and(|ps|ps.iter().any(|p|p["HostPort"].as_str()==Some(&port.to_string())&&matches!(p["HostIp"].as_str(),Some("127.0.0.1")|Some("0.0.0.0"))));}else{return Err("preflight refusal: unknown role".into());}
            containers.push(v);receipt["containers"]=json!(containers);super::write_json(&path,&receipt)?;
        }
        receipt["roles"]=json!(roles);receipt["endpoint_matches"]=json!(endpoint_matches);
        if roles!=[3,3,1]||!endpoint_matches||networks.len()!=1||pd.len()!=3||tikv.len()!=3{return Err("preflight refusal: topology mismatch".into());}
        let network_id=networks.iter().next().unwrap();
        let raw=docker(commands,&["network","inspect",network_id,"--format",r#"{"id":{{json .Id}},"run":{{json (index .Labels "mount-rs.tidb.run")}},"driver":{{json .Driver}}}"#]).await?;
        receipt["network"]=serde_json::from_str(&raw).map_err(|_|"network inspect invalid")?;
        if receipt["network"]["id"]!=*network_id||receipt["network"]["run"]!=run||receipt["network"]["driver"]!="bridge"{return Err("preflight refusal: network identity mismatch".into());}
        super::write_json(&path,&receipt)?;
        if receipt["docker_info"]["memory_bytes"].as_u64().unwrap_or(0)<10737418240||receipt["docker_info"]["cpus"].as_u64().unwrap_or(0)<4{return Err("preflight refusal: durable TiDB requires measured10GiB Docker VM memory and4 CPUs".into());}
        for (field,endpoint) in [("pd_members","members"),("pd_health","health"),("tikv_stores","stores")]{
            let url=format!("http://127.0.0.1:2379/pd/api/v1/{endpoint}");
            let raw=docker(commands,&["exec",&pd_id,"curl","-fsS","--max-time","3",&url]).await?;
            receipt[field]=serde_json::from_str(&raw).map_err(|_|"cluster health response invalid")?;super::write_json(&path,&receipt)?;
        }
        validate_health(&receipt["pd_members"],&receipt["pd_health"],&receipt["tikv_stores"],&pd,&tikv)?;
        owner.pool=Some(mysql_async::Pool::from_url(&connection).map_err(|_|"SQL options invalid (redacted)")?);
        let rows:Vec<(String,String,String)>=tokio::time::timeout(Duration::from_secs(10),async{
            let mut conn=owner.pool.as_ref().unwrap().get_conn().await.map_err(|_|"SQL readiness connection failed")?;
            conn.query("SELECT TYPE, INSTANCE, VERSION FROM information_schema.cluster_info").await.map_err(|_|"SQL cluster identity unavailable")
        }).await.map_err(|_|"SQL readiness deadline")??;
        let observed:Vec<_>=rows.into_iter().map(|(role,address,version)|json!({"role":role,"address":address,"version":version})).collect();
        for (role,expected) in [("pd",&pd),("tikv",&tikv),("tidb",&sql)]{
            let selected:Vec<_>=observed.iter().filter(|r|r["role"]==role).cloned().collect();
            if selected.len()!=expected.len()||addresses(&json!(selected),"address")?!=*expected{return Err("SQL cluster identity differs from inspected fixture".into());}
        }
        if observed.len()!=7||observed.iter().any(|r|r["version"].as_str().is_none_or(str::is_empty)){return Err("SQL cluster role/version incomplete".into());}
        receipt["sql_cluster"]=json!(observed);receipt["qualified"]=json!(true);receipt["complete"]=json!(true);Ok::<_,String>(())
    }.await;
    receipt["error"] = json!(result.as_ref().err());
    receipt["completed_unix_ms"] = json!(super::utc_ms());
    super::write_json(&path, &receipt)?;
    result?;
    Ok(receipt)
}
#[test]
fn remote_daemons_and_unknown_cluster_members_fail_closed() {
    for value in [
        "tcp://localhost:2375",
        "ssh://remote",
        "",
        "unix://relative",
    ] {
        assert!(local_endpoint(value).is_err());
    }
    local_endpoint("unix:///var/run/docker.sock").unwrap();
    assert!(
        validate_health(
            &json!({}),
            &json!([]),
            &json!({}),
            &BTreeSet::new(),
            &BTreeSet::new()
        )
        .is_err()
    );
}
#[test]
fn membership_requires_exact_inspected_addresses_and_all_up() {
    let pd: BTreeSet<_> = (1..=3).map(|i| format!("10.0.0.{i}:2379")).collect();
    let tikv: BTreeSet<_> = (4..=6).map(|i| format!("10.0.0.{i}:20160")).collect();
    let members = json!({"members":pd.iter().map(|address|json!({"client_urls":[format!("http://{address}")]})).collect::<Vec<_>>()});
    let health = json!(
        pd.iter()
            .map(|address| json!({"health":true,"client_urls":[format!("http://{address}")]}))
            .collect::<Vec<_>>()
    );
    let mut stores = json!({"stores":tikv.iter().map(|address|json!({"store":{"state_name":"Up","address":address}})).collect::<Vec<_>>()});
    validate_health(&members, &health, &stores, &pd, &tikv).unwrap();
    stores["stores"][0]["store"]["state_name"] = json!("Down");
    assert!(validate_health(&members, &health, &stores, &pd, &tikv).is_err());
    stores["stores"][0]["store"]["state_name"] = json!("Up");
    stores["stores"][0]["store"]["address"] = json!("10.1.0.1:20160");
    assert!(validate_health(&members, &health, &stores, &pd, &tikv).is_err());
}
