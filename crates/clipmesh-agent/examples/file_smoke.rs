//! Explicit operator smoke test. Publishing changes connected desktop clipboards.
use clipmesh_agent::{files::FileTransport, AgentConfig};
use clipmesh_protocol::{files::FileDescriptor, UuidV4};
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::args()
        .nth(1)
        .ok_or("expected hub WebSocket URL")?;
    let config = AgentConfig::parse_toml(&format!(
        "config_version=1\nhub_url={endpoint:?}\nplatform=\"linux-wayland\"\nstate_path=\"/tmp/clipmesh-smoke-unused/state\"\ncontrol_socket=\"/tmp/clipmesh-smoke-unused/control\""
    ))?;
    let bytes = b"ClipMesh synthetic file transfer check\n\0\xff\x80".to_vec();
    let id = UuidV4::new();
    let name = format!("clipmesh-check-{}.bin", id.get());
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let descriptor = FileDescriptor {
        name: name.clone(),
        media_type: "application/octet-stream".into(),
        size_bytes: bytes.len() as u64,
        sha256: hash.clone(),
    };
    let mut client = FileTransport::connect(&config)?;
    client.publish(id.clone(), &[(descriptor, bytes.clone())])?;
    let clip = client
        .history()?
        .into_iter()
        .find(|clip| clip.clip_id == id)
        .ok_or("published file absent")?;
    if client.download(&clip, 0)? != bytes {
        return Err("download mismatch".into());
    }
    println!("verified {name} sha256={hash}");
    Ok(())
}
