use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::sync::{Arc, Mutex};
use anyhow::Error;
use serde::{Deserialize, Serialize};
use home::env::Env;
use home::env::OS_ENV;
use openssl::x509::store::{X509Store, X509StoreBuilder, X509StoreRef};
use openssl::x509::X509;
use rustc_hash::FxHashMap;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectionEntry {
    pub address: String,
    #[serde(rename="heapSize")]
    pub heap_size: String,
    pub icon: String,
    pub id: String,
    #[serde(rename="javaHome")]
    pub java_home: String,
    pub name: String,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default = "get_verify")]
    pub verify: bool
}

pub struct ConnectionStore {
    cache: Mutex<HashMap<String, Arc<ConnectionEntry>>>,
    con_location: PathBuf,
    cert_store: X509Store,
    trusted_certs_location: PathBuf
}

impl Default for ConnectionEntry {
    fn default() -> Self {
        let empty_str = String::from("");
        ConnectionEntry{address: empty_str.clone(), heap_size: String::from("512m"), icon: empty_str.clone(),
        id: Uuid::new_v4().to_string(), java_home: find_java_home(), name: empty_str.clone(), username: None,
        password: None, verify: true}
    }
}

impl ConnectionStore {
    pub fn init(data_dir_path: PathBuf) -> Result<Self, Error> {
        let con_location = data_dir_path.join("catapult-data.json");
        let mut con_location_file = File::open(&con_location);
        if let Err(e) = con_location_file {
            con_location_file = File::create(&con_location);
        }
        let con_location_file = con_location_file?;

        let mut cache = HashMap::new();
        let data : serde_json::Result<HashMap<String, ConnectionEntry>> = serde_json::from_reader(con_location_file);
        if let Ok(data) = data {
            for (id, ce) in data {
                cache.insert(id, Arc::new(ce));
            }
        }
        else {
            println!("{}", data.err().unwrap().to_string());
        }

        let trusted_certs_location = data_dir_path.join("trusted-certs.json");
        let certs = parse_trusted_certs(&trusted_certs_location);
        let cert_store = create_cert_store(certs);
        // if let Err(e) = trusted_certs_location_file {
        //     trusted_certs_location_file = File::create(&trusted_certs_location);
        // }
        // let trusted_certs_location_file = trusted_certs_location_file?;

        Ok(ConnectionStore{ con_location, cache: Mutex::new(cache), cert_store, trusted_certs_location })
    }

    pub fn to_json_array_string(&self) -> String {
        let mut sb = String::with_capacity(1024);
        let len = self.cache.lock().unwrap().len();
        sb.push('[');
        for (pos, ce) in self.cache.lock().unwrap().values().enumerate() {
            let c = serde_json::to_string(ce).expect("failed to serialize ConnectionEntry");
            sb.push_str(c.as_str());
            if pos + 1 < len {
                sb.push(',');
            }
        }
        sb.push(']');

        sb
    }

    pub fn get(&self, id: &str) -> Option<Arc<ConnectionEntry>> {
        let cs = self.cache.lock().unwrap();
        let val = cs.get(id);
        if let Some(val) = val {
            return Some(Arc::clone(val));
        }
        None
    }

    pub fn save(&self, mut ce: ConnectionEntry) -> Result<String, Error> {
        if ce.id.is_empty() {
            ce.id = uuid::Uuid::new_v4().to_string();
        }

        let mut jh = ce.java_home.trim().to_string();
        if jh.is_empty() {
            jh = find_java_home();
        }
        ce.java_home = jh;

        if let Some(ref username) = ce.username {
            let username = username.trim();
            if username.is_empty() {
                ce.username = None;
            }
        }

        if let Some(ref password) = ce.password {
            let password = password.trim();
            if password.is_empty() {
                ce.username = None;
            }
        }

        let data = serde_json::to_string(&ce)?;
        self.cache.lock().unwrap().insert(ce.id.clone(), Arc::new(ce));
        self.flush_to_disk()?;
        Ok(data)
    }

    pub fn delete(&self, id: &str) -> Result<(), Error> {
        self.cache.lock().unwrap().remove(id);
        self.flush_to_disk()?;
        Ok(())
    }

    pub fn import(&self, file_path: &str) -> Result<String, Error> {
        let f = File::open(file_path)?;
        let data: Vec<ConnectionEntry> = serde_json::from_reader(f)?;
        let mut count = 0;
        let java_home = find_java_home();
        for mut ce in data {
            ce.java_home = java_home.clone();
            self.cache.lock().unwrap().insert(ce.id.clone(), Arc::new(ce));
            count = count + 1;
        }

        self.flush_to_disk()?;
        Ok(format!("imported {} connections", count))
    }

    pub fn add_trusted_cert(&mut self, cert_der: &str) -> Result<(), Error> {
        let mut certs = parse_trusted_certs(&self.trusted_certs_location);
        let cert_der = openssl::base64::decode_block(cert_der)?;
        let cert = X509::from_der(cert_der.as_slice())?;
        certs.push(cert);

        let mut der_certs = Vec::with_capacity(certs.len());
        for c in &certs {
            let der = c.to_der()?;
            let der = openssl::base64::encode_block(der.as_slice());
            der_certs.push(der);
        }
        let val = serde_json::to_string(&der_certs)?;
        let mut f = OpenOptions::new().append(false).write(true).open(&self.trusted_certs_location)?;
        f.write_all(val.as_bytes())?;

        let new_store = create_cert_store(certs);
        self.cert_store = new_store;
        Ok(())
    }

    pub fn get_cert_store(&self) -> &X509StoreRef {
        self.cert_store.as_ref()
    }

    fn flush_to_disk(&self) -> Result<(), Error> {
        let val = serde_json::to_string(&self.cache)?;
        let mut f = OpenOptions::new().append(false).write(true).open(&self.con_location);
        if let Err(e) = f {
            println!("unable to open file for writing: {}", e.to_string());
            return Err(Error::new(e));
        }
        f.unwrap().write_all(val.as_bytes())?;
        Ok(())
    }
}

pub fn find_java_home() -> String {
    let mut java_home = String::from("");
    if let Some(jh) = OS_ENV.var_os("JAVA_HOME") {
        java_home = String::from(jh.to_str().unwrap());
        println!("JAVA_HOME is set to {}", java_home);
    }

    if java_home.is_empty() {
        let out = Command::new("/usr/libexec/java_home").args(["-v", "1.8"]).output();
        if let Ok(out) = out {
            if out.status.success() {
                java_home = String::from_utf8(out.stdout).expect("failed to create UTF-8 string from OsStr");
                println!("/usr/libexec/java_home -v 1.8 returned {}", java_home);
            }
        }
    }
    java_home
}

fn parse_trusted_certs(trusted_certs_location: &PathBuf) -> Vec<X509> {
    let mut certs = Vec::new();
    let mut trusted_certs_location_file = File::open(trusted_certs_location);
    if let Ok(trusted_certs_location_file) = trusted_certs_location_file {
        let cert_map : serde_json::Result<FxHashMap<String, String>> = serde_json::from_reader(trusted_certs_location_file);
        if let Ok(cert_map) = cert_map {
            for (key, der_data) in cert_map {
                let der_data = openssl::base64::decode_block(&der_data);
                if let Ok(der_data) = der_data {
                    let c = X509::from_der(der_data.as_slice());
                    if let Ok(c) = c {
                        certs.push(c);
                    }
                    else {
                        println!("failed to parse cert from DER data with key {} {:?}", key, c.err());
                    }
                }
                else {
                    println!("invalid base64 encoded data with key {} {:?}", key, der_data.err());
                }
            }
        }
        else {
            println!("failed to parse trusted certificates JSON file {:?} {:?}", trusted_certs_location, cert_map.err());
        }
    }

    println!("found {} trusted certificates", certs.len());
    certs
}

fn create_cert_store(certs: Vec<X509>) -> X509Store {
    if !openssl_probe::has_ssl_cert_env_vars() {
        println!("probing and setting OpenSSL environment variables");
        openssl_probe::init_ssl_cert_env_vars();
    }
    let mut cert_store_builder = X509StoreBuilder::new().expect("unable to created X509 store builder");
    cert_store_builder.set_default_paths().expect("failed to load system default trusted certs");
    for c in certs {
        cert_store_builder.add_cert(c).expect("failed to add a cert to the in-memory store");
    }

    cert_store_builder.build()
}

fn get_verify() -> bool {
    //println!("getting default value for verify attribute");
    true
}