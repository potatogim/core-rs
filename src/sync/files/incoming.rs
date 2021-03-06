use ::std::sync::{Arc, RwLock, Mutex};
use ::sync::{SyncConfig, Syncer};
use ::sync::sync_model::SyncModel;
use ::storage::Storage;
use ::api::{Api, Method};
use ::messaging;
use ::error::{TResult, TError};
use ::models::sync_record::{SyncType, SyncRecord};
use ::models::file::FileData;
use ::std::time::Duration;
use ::std::fs;
use ::std::io::{Read, Write};
use ::jedi::{self, Value};
use ::util;
use ::config;
use ::reqwest;

/// Holds the state for incoming files (download)
pub struct FileSyncIncoming {
    /// Holds our sync config. Note that this is shared between the sync system
    /// and the `Turtl` object in the main thread.
    config: Arc<RwLock<SyncConfig>>,

    /// Holds our Api object. Lets us chit chat with the Turtl server.
    api: Arc<Api>,

    /// Holds our user-specific db. This is mainly for persisting k/v data and
    /// for polling for file records that need downloading.
    db: Arc<Mutex<Option<Storage>>>,

    /// Stores our syn run version
    run_version: i64,
}

impl FileSyncIncoming {
    /// Create a new incoming syncer
    pub fn new(config: Arc<RwLock<SyncConfig>>, api: Arc<Api>, db: Arc<Mutex<Option<Storage>>>) -> Self {
        FileSyncIncoming {
            config: config,
            api: api,
            db: db,
            run_version: 0,
        }
    }

    /// Returns a list of note_ids for notes that have pending file downloads.
    /// This uses the `sync` table.
    fn get_incoming_file_syncs(&self) -> TResult<Vec<SyncRecord>> {
        let syncs = with_db!{ db, self.db,
            SyncRecord::find(db, Some(SyncType::FileIncoming))
        }?;
        let mut final_syncs = Vec::with_capacity(syncs.len());
        for sync in syncs {
            // NOTE: in the normal sync process, we break on frozen. here, we
            // continue. the reason being that file syncs don't necessarily
            // benefit from being run in order like normal outgoing syncs do.
            if sync.frozen { continue; }
            final_syncs.push(sync);
        }
        Ok(final_syncs)
    }

    /// Given a sync record for an outgoing file, find the corresponding file
    /// in our storage folder and stream it to our heroic API.
    fn download_file(&mut self, sync: &SyncRecord) -> TResult<()> {
        let note_id = &sync.item_id;
        let user_id = {
            let local_config = self.get_config();
            let guard = lockr!(local_config);
            match guard.user_id.as_ref() {
                Some(x) => x.clone(),
                None => return TErr!(TError::MissingField(String::from("SyncConfig.user_id"))),
            }
        };
        info!("FileSyncIncoming.download_file() -- syncing file for {}", note_id);

        // define a container function that grabs our file and runs the download.
        // if anything in here fails, we mark 
        let download = |note_id, user_id| -> TResult<()> {
            // generate the filename we'll save to, and open the file (we should
            // test if the file can be created before we run off blasting API
            // calls in every direction)
            let file = FileData::new_file(user_id, note_id)?;
            let parent = match file.parent() {
                Some(path) => path.clone(),
                None => return TErr!(TError::BadValue(format!("bad file path: {:?}", file))),
            };
            util::create_dir(parent)?;
            let mut file = fs::File::create(&file)?;

            // start our API call to the note file attachment endpoint
            let url = format!("/notes/{}/attachment", note_id);
            // grab the location of the file we'll be downloading
            let file_url: String = self.api.get(&url[..])?.call()?;
            info!("FileSyncIncoming.download_file() -- grabbing file at URL {}", file_url);

            let mut client_builder = reqwest::Client::builder()
                .timeout(Duration::new(30, 0));
            match config::get::<Option<String>>(&["api", "proxy"]) {
                Ok(Some(proxy_cfg)) => {
                    client_builder = client_builder.proxy(reqwest::Proxy::http(format!("http://{}", proxy_cfg).as_str())?);
                }
                Ok(None) => {}
                Err(_) => {}
            }
            let client = client_builder.build()?;
            let req = client.request(Method::GET, reqwest::Url::parse(file_url.as_str())?);
            // only add our auth junk if we're calling back to the turtl api!
            let turtl_api_url: String = config::get(&["api", "endpoint"])?;
            let req = if file_url.contains(turtl_api_url.as_str()) {
                self.api.set_auth_headers(req)
            } else {
                req
            };
            let mut res = client.execute(req.build()?)?;
            if res.status().as_u16() >= 400 {
                let errstr = res.text()?;
                let val = match jedi::parse(&errstr) {
                    Ok(x) => x,
                    Err(_) => Value::String(errstr),
                };
                return TErr!(TError::Api(res.status(), val));
            }
            // start streaming our API call into the file 4K at a time
            let mut buf = [0; 4096];
            loop {
                let read = res.read(&mut buf[..])?;
                // all done! (EOF)
                if read <= 0 { break; }
                let (read_bytes, _) = buf.split_at(read);
                let written = file.write(read_bytes)?;
                if read != written {
                    return TErr!(TError::Msg(format!("problem downloading file: downloaded {} bytes, only saved {} wtf wtf lol", read, written)));
                }
            }
            Ok(())
        };

        match download(&note_id, &user_id) {
            Ok(_) => {}
            Err(e) => {
                // our download failed? send to our sync failure handler
                with_db!{ db, self.db,
                    SyncRecord::handle_failed_sync(db, sync)?;
                };
                return Err(e);
            }
        }

        // if we're still here, the download succeeded. remove the sync record so
        // we know to stop trying to download this file.
        with_db!{ db, self.db, sync.db_delete(db, None)? };

        // let the UI know how great we are. you will love this app. tremendous
        // app. everyone says so.
        messaging::ui_event("sync:file:downloaded", &json!({"note_id": note_id}))?;
        Ok(())
    }
}

impl Syncer for FileSyncIncoming {
    fn get_name(&self) -> &'static str {
        "files:incoming"
    }

    fn get_config(&self) -> Arc<RwLock<SyncConfig>> {
        self.config.clone()
    }

    fn get_delay(&self) -> u64 {
        1000
    }

    fn set_run_version(&mut self, run_version: i64) {
        self.run_version = run_version;
    }

    fn get_run_version(&self) -> i64 {
        self.run_version
    }

    fn run_sync(&mut self) -> TResult<()> {
        let syncs = self.get_incoming_file_syncs()?;
        for sync in &syncs {
            self.download_file(sync)?;
            // if we've been disabled, return
            if !self.is_enabled() { return Ok(()); }
        }
        Ok(())
    }
}


