use super::{
    config::REQUEST_SECONDS,
    fixture::{FileProfile, oracle_block},
    state::{Counts, Expected},
    wire,
};
use mount_rs_remote_protocol::{OperationName, binary::IoRequest};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
pub struct Lane {
    pub connection: quinn::Connection,
    pub expected: Expected,
    pub counts: std::sync::Arc<std::sync::Mutex<Counts>>,
    pub generation: u64,
}
impl Lane {
    pub fn new(connection: quinn::Connection, drive: usize, files: usize) -> Self {
        Self {
            connection,
            expected: Expected::empty(drive, files),
            counts: std::sync::Arc::new(std::sync::Mutex::new(Counts::default())),
            generation: 0,
        }
    }
    pub async fn request(
        &mut self,
        operation: OperationName,
        body: Value,
    ) -> Result<Value, String> {
        let id = self.counts.lock().unwrap().begin();
        let began = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(REQUEST_SECONDS),
            wire::request(
                &self.connection,
                id,
                &format!("sandbox-{}", self.expected.drive),
                operation,
                body,
            ),
        )
        .await;
        self.latency(began);
        match result {
            Ok(Ok(Ok(value))) => {
                self.counts.lock().unwrap().acknowledge(id)?;
                Ok(value)
            }
            Ok(Ok(Err(code))) => {
                self.counts.lock().unwrap().failed();
                Err(format!("protocol operation rejected: {code}"))
            }
            _ => {
                self.counts.lock().unwrap().uncertain();
                self.connection
                    .close(1u32.into(), b"uncertain operation; never replayed");
                Err("operation outcome unknown; never replayed".into())
            }
        }
    }
    fn latency(&mut self, began: Instant) {
        let us = began.elapsed().as_micros().min(u64::MAX as u128) as u64;
        let mut counts = self.counts.lock().unwrap();
        counts.latency_us += us;
        let bucket = (64 - us.max(1).leading_zeros() as usize).min(31);
        counts.latency_histogram_log2_us[bucket] += 1;
        counts.latency_max_us = counts.latency_max_us.max(us);
    }
    pub async fn open(&mut self, name: &str, create: bool, file: usize) -> Result<u64, String> {
        let handle = self
            .request(
                OperationName::Open,
                json!({"path":format!("/{name}"),"flags":if create {"wx+"}else{"r+"},"mode":420}),
            )
            .await?
            .as_u64()
            .ok_or("invalid acknowledged handle")?;
        if create {
            self.expected.create(name.into(), file);
        }
        Ok(handle)
    }
    pub async fn close(&mut self, handle: u64) -> Result<(), String> {
        self.request(OperationName::HandleClose, json!({"handle":handle}))
            .await?;
        Ok(())
    }
    pub async fn write(
        &mut self,
        handle: u64,
        name: &str,
        block: usize,
        generation: u64,
    ) -> Result<(), String> {
        let file = self.expected.files[name].identity;
        // Byte preparation precedes the measured transport request.
        let data = oracle_block(
            (self.expected.drive / 2) as u64,
            self.expected.drive as u64,
            file as u64,
            block as u64,
            generation,
        );
        let request = IoRequest {
            drive_id: format!("sandbox-{}", self.expected.drive),
            handle,
            position: Some((block * 4096) as u64),
        };
        let id = self.counts.lock().unwrap().begin();
        let began = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(REQUEST_SECONDS),
            wire::handle_write(&self.connection, id, &request, &data),
        )
        .await;
        self.latency(began);
        if matches!(result, Ok(Ok(4096))) {
            self.counts.lock().unwrap().acknowledge(id)?;
            self.expected.write(name, block, generation);
            Ok(())
        } else {
            self.counts.lock().unwrap().uncertain();
            Err("write incomplete or unknown; never replayed".into())
        }
    }
    pub async fn read(&mut self, handle: u64, name: &str, block: usize) -> Result<(), String> {
        let expected = self.expected.bytes(name, block);
        let mut data = [0; 4096];
        let request = IoRequest {
            drive_id: format!("sandbox-{}", self.expected.drive),
            handle,
            position: Some((block * 4096) as u64),
        };
        let id = self.counts.lock().unwrap().begin();
        let began = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(REQUEST_SECONDS),
            wire::handle_read(&self.connection, id, &request, &mut data),
        )
        .await;
        self.latency(began);
        if matches!(result, Ok(Ok(4096))) {
            self.counts.lock().unwrap().acknowledge(id)?;
            if data != expected {
                return Err("online every-byte oracle mismatch".into());
            }
            Ok(())
        } else {
            self.counts.lock().unwrap().uncertain();
            Err("read incomplete or unknown".into())
        }
    }
    pub async fn populate(&mut self, files: usize, payload: bool) -> Result<(), String> {
        for file in 0..files {
            let name = format!("mixed-{file}");
            let handle = self.open(&name, !payload, file).await?;
            if payload {
                for block in 0..FileProfile::Mixed.size(file) / 4096 {
                    self.write(handle, &name, block, 0).await?;
                }
            }
            self.close(handle).await?;
        }
        Ok(())
    }
    pub async fn cycle(
        &mut self,
        pattern: &str,
        sequence: usize,
        files: usize,
    ) -> Result<(), String> {
        if pattern == "churn" {
            let from = "churn-new";
            let to = "churn-renamed";
            let handle = self.open(from, true, 0).await?;
            self.close(handle).await?;
            self.request(
                OperationName::Rename,
                json!({"path":format!("/{from}"),"destination":format!("/{to}")}),
            )
            .await?;
            self.expected.rename(from, to.into());
            self.request(OperationName::Unlink, json!({"path":format!("/{to}")}))
                .await?;
            self.expected.delete(to);
            return Ok(());
        }
        let random = sequence
            .wrapping_mul(2654435761)
            .wrapping_add(self.expected.drive * 7919);
        let file = match pattern {
            "append_truncate" => 0,
            "hot_file" if !sequence.is_multiple_of(10) => 0,
            "random_read" | "random_overwrite" | "mixed" | "hot_file" => random % files,
            _ => sequence % files,
        };
        let name = format!("mixed-{file}");
        let handle = self.open(&name, false, file).await?;
        let blocks = self.expected.files[&name].length / 4096;
        let block = if pattern.starts_with("sequential") {
            (sequence / files) % blocks
        } else {
            (random / files) % blocks
        };
        match pattern {
            "sequential_read" | "random_read" => self.read(handle, &name, block).await?,
            "mixed" if sequence.is_multiple_of(2) => self.read(handle, &name, block).await?,
            "hot_file" if sequence.is_multiple_of(4) => self.read(handle, &name, block).await?,
            "append_truncate" if blocks > 1 => {
                self.request(
                    OperationName::HandleTruncate,
                    json!({"handle":handle,"length":4096}),
                )
                .await?;
                self.expected.truncate(&name, 4096);
            }
            "append_truncate" => {
                self.generation += 1;
                self.write(handle, &name, blocks, self.generation).await?;
            }
            _ => {
                self.generation += 1;
                self.write(handle, &name, block, self.generation).await?;
            }
        }
        self.close(handle).await
    }
}
pub fn endpoint(cert: &[u8]) -> Result<quinn::Endpoint, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(cert.to_vec()))
        .map_err(|_| "TLS root invalid")?;
    let mut tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "TLS versions unavailable")?
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"mount-rs/2".to_vec()];
    let mut config = quinn::ClientConfig::new(std::sync::Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|_| "QUIC TLS unavailable")?,
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(15)));
    config.transport_config(std::sync::Arc::new(transport));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap())
        .map_err(|_| "client endpoint bind failed")?;
    endpoint.set_default_client_config(config);
    Ok(endpoint)
}
