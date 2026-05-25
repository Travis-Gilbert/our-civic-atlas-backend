pub mod civic_atlas {
    pub mod v1 {
        tonic::include_proto!("civic_atlas.v1");
    }
}

pub mod theseus_bridge {
    pub mod v1 {
        tonic::include_proto!("theseus_bridge.v1");
    }
}

pub mod theseus_search {
    pub mod v1 {
        tonic::include_proto!("theseus_search.v1");
    }
}

pub mod event_planner;
