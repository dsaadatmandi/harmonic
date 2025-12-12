use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::utils::Result;

struct FingerprintStore {
    path: PathBuf,
    fingerprints: Vec<Fingerprint>,
}

#[derive(Serialize, Deserialize)]
struct Fingerprint {
    hash: [u8; 32],
}

impl FingerprintStore {
    pub fn load(path: &PathBuf) -> Self {
        let mut store: Vec<Fingerprint> = Vec::new();
        for entry in WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|f| {
                f.file_name()
                    .to_str()
                    .map(|s| s.ends_with(".prnt"))
                    .unwrap_or(false)
            })
        {
            match fs::File::open(entry.into_path()) {
                Ok(f) => match serde_json::from_reader(f) {
                    Ok(fingerprint) => store.push(fingerprint),
                    Err(_) => continue,
                },
                Err(_) => continue,
            }
        }
        Self {
            path: path.to_path_buf(),
            fingerprints: store,
        }
    }

    pub fn check(&self, hash: [u8; 32]) -> bool {
        self.fingerprints.iter().any(|f| f.hash == hash)
    }

    pub fn store(&self, hash: [u8; 32]) -> Result<()> {
        if !Self::trigger_user_verification() {
            return Ok(())
        }
        let fp = Fingerprint { hash };
        let mut pout = self.path.join(uuid::Uuid::new_v4().to_string());
        pout.set_extension(".prnt");
        let fout = fs::File::create(pout)?;

        serde_json::to_writer(fout, &fp)?;
        Ok(())
    }

    pub fn trigger_user_verification() -> bool {
        // WORK IN PROGRESS
        println!("Unknown fingerprint encountered");
        println!("Information:");

        println!("Should the certificate be saved? (y/n)");

        true
    }
}


