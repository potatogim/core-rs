use ::jedi::Value;

use ::models::model::Model;
use ::models::protected::{Keyfinder, Protected};
use ::models::keychain::Keychain;
use ::turtl::Turtl;

protected! {
    #[derive(Serialize, Deserialize)]
    pub struct Board {
        #[protected_field(public)]
        pub user_id: String,
        #[protected_field(public)]
        pub space_id: String,
        #[protected_field(public)]
        pub meta: Option<Value>,

        #[protected_field(private)]
        pub title: Option<String>,
    }
}

make_storable!(Board, "boards");
make_basic_sync_model!(Board);

impl Keyfinder for Board {
    fn get_key_search(&self, turtl: &Turtl) -> Keychain {
        let mut keychain = Keychain::new();
        let mut space_ids: Vec<String> = Vec::new();
        let mut board_ids: Vec<String> = Vec::new();
        if self.space_id.is_some() {
            space_ids.push(self.space_id.as_ref().unwrap().clone());
        }
        match self.keys.as_ref() {
            Some(keys) => for key in keys {
                match key.get(&String::from("s")) {
                    Some(id) => space_ids.push(id.clone()),
                    None => {},
                }
                match key.get(&String::from("b")) {
                    Some(id) => board_ids.push(id.clone()),
                    None => {},
                }
            },
            None => {},
        }

        let user_id = String::from("");     // fake id is ok
        if space_ids.len() > 0 {
            let ty = String::from("space");
            let profile_guard = turtl.profile.read().unwrap();
            for space in &profile_guard.spaces {
                if space.id().is_none() || space.key().is_none() { continue; }
                let space_id = space.id().unwrap();
                if !space_ids.contains(space_id) { continue; }
                keychain.add_key(&user_id, space_id, space.key().unwrap(), &ty);
            }
        }
        if board_ids.len() > 0 {
            let ty = String::from("board");
            let profile_guard = turtl.profile.read().unwrap();
            for board in &profile_guard.boards {
                if board.id().is_none() || board.key().is_none() { continue; }
                let board_id = board.id().unwrap();
                if !board_ids.contains(board_id) { continue; }
                keychain.add_key(&user_id, board_id, board.key().unwrap(), &ty);
            }
        }
        keychain
    }
}

