use std::fmt;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::transaction_tracker::TransactionTracker;
use crate::vfs::NFSFileSystem;

#[derive(Clone, Debug)]
pub struct RpcAuthRequest {
    pub client_addr: String,
    pub auth_flavor: crate::auth_flavor,
    pub auth: crate::auth_unix,
    pub program: u32,
    pub procedure: u32,
}

pub trait RpcAuthorizer: Send + Sync {
    fn authorize(&self, request: &RpcAuthRequest) -> bool;
}

#[derive(Clone)]
pub struct RPCContext {
    pub local_port: u16,
    pub client_addr: String,
    pub auth_flavor: crate::auth_flavor,
    pub auth: crate::auth_unix,
    pub vfs: Arc<dyn NFSFileSystem + Send + Sync>,
    pub mount_signal: Option<mpsc::Sender<bool>>,
    pub export_name: Arc<String>,
    pub authorizer: Option<Arc<dyn RpcAuthorizer>>,
    pub transaction_tracker: Arc<TransactionTracker>,
}

impl RPCContext {
    pub fn is_authorized(&self, program: u32, procedure: u32) -> bool {
        let Some(authorizer) = &self.authorizer else {
            return true;
        };
        let request = RpcAuthRequest {
            client_addr: self.client_addr.clone(),
            auth_flavor: self.auth_flavor,
            auth: self.auth.clone(),
            program,
            procedure,
        };
        authorizer.authorize(&request)
    }
}

impl fmt::Debug for RPCContext {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("RPCContext")
            .field("local_port", &self.local_port)
            .field("client_addr", &self.client_addr)
            .field("auth_flavor", &self.auth_flavor)
            .field("auth", &self.auth)
            .finish()
    }
}
