// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        account::{Account, AccountContext},
        common::AccountLifetime,
        inspect, lock_request, pre_auth,
    },
    account_common::{AccountId, AccountManagerError, FidlAccountId},
    anyhow::format_err,
    fidl::endpoints::{create_endpoints, ServerEnd},
    fidl::prelude::*,
    fidl_fuchsia_identity_account::{AccountMarker, Error as ApiError},
    fidl_fuchsia_identity_authentication::{
        Enrollment, InteractionMarker, InteractionProtocolServerEnd, StorageUnlockMechanismMarker,
        StorageUnlockMechanismProxy,
    },
    fidl_fuchsia_identity_internal::{
        AccountHandlerControlRequest, AccountHandlerControlRequestStream,
    },
    fuchsia_async as fasync,
    fuchsia_component::client,
    fuchsia_inspect::{Inspector, Property},
    futures::{channel::oneshot, lock::Mutex, prelude::*},
    identity_common::TaskGroupError,
    lazy_static::lazy_static,
    log::{error, info, warn},
    std::{collections::HashMap, fmt, sync::Arc},
};

lazy_static! {
    /// Temporary pre-key material which constitutes a successful authentication
    /// attempt, for manual and automatic tests. This constant is specifically
    /// for the developer authenticator implementations
    /// (see src/identity/bin/dev_authenticator) and needs to stay in sync.
    static ref MAGIC_PREKEY: Vec<u8>  = vec![77; 32];

    static ref DEV_AUTHENTICATION_MECHANISM_PATHS: HashMap<&'static str, &'static str> = HashMap::from(
        [
            (
                "#meta/dev_authenticator_always_succeed.cm",
                "/svc/fuchsia.identity.authentication.AlwaysSucceedStorageUnlockMechanism"
            ),
            (
                "#meta/dev_authenticator_always_fail_authentication.cm",
                "/svc/fuchsia.identity.authentication.AlwaysFailStorageUnlockMechanism"
            )
        ]);
}

// A static enrollment id which represents the only enrollment.
const ENROLLMENT_ID: u64 = 0;

/// The states of an AccountHandler.
enum Lifecycle {
    /// An account has not yet been created or loaded.
    Uninitialized,

    /// The account is locked.
    Locked,

    /// The account is currently loaded and is available.
    Initialized { account: Arc<Account> },

    /// There is no account present, and initialization is not possible.
    Finished,
}

impl fmt::Debug for Lifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            &Lifecycle::Uninitialized => "Uninitialized",
            &Lifecycle::Locked { .. } => "Locked",
            &Lifecycle::Initialized { .. } => "Initialized",
            &Lifecycle::Finished => "Finished",
        };
        write!(f, "{}", name)
    }
}

/// The pre-authentication state for an account.
struct AccountPreAuthState {
    // TODO(fxbug.dev/104516): Populate and use AccountPreAuthState. This should probably
    // be implemented in the pre_auth module rather than as the separate struct defined here.
}

impl From<Vec<u8>> for AccountPreAuthState {
    fn from(_data: Vec<u8>) -> Self {
        // TODO(fxbug.dev/104516): Deserialize AccountPreAuthState. This should probably be
        // implemented using serde.
        AccountPreAuthState {}
    }
}

impl Into<Vec<u8>> for AccountPreAuthState {
    fn into(self) -> Vec<u8> {
        // TODO(fxbug.dev/104516): Serialize AccountPreAuthState. This should probably be
        // implemented using serde.
        vec![]
    }
}

/// The core state of the AccountHandler, i.e. the Account (once it is known) and references to
/// the execution context.
pub struct AccountHandler {
    /// The current state of the AccountHandler state machine, optionally containing
    /// a reference to the `Account` and its pre-authentication data, depending on the
    /// state. The methods of the AccountHandler drives changes to the state.
    state: Arc<Mutex<Lifecycle>>,

    /// Lifetime for this account (ephemeral or persistent with a path).
    lifetime: AccountLifetime,

    /// An implementation of the `Manager` trait that is responsible for retrieving and
    /// persisting pre-authentication data of an account. The pre-auth data is cached
    /// and available in the Locked and Initialized states.
    // TODO(dnordstrom): Consider moving the pre_auth_manager into the AccountLifetime enum.
    pre_auth_manager: Arc<dyn pre_auth::Manager>,

    /// Helper for outputting account handler information via fuchsia_inspect.
    inspect: Arc<inspect::AccountHandler>,
}

impl AccountHandler {
    /// Constructs a new AccountHandler and puts it in the Uninitialized state.
    pub fn new(
        account_id: AccountId,
        lifetime: AccountLifetime,
        pre_auth_manager: Arc<dyn pre_auth::Manager>,
        inspector: &Inspector,
    ) -> AccountHandler {
        let inspect =
            Arc::new(inspect::AccountHandler::new(inspector.root(), &account_id, "uninitialized"));
        Self {
            state: Arc::new(Mutex::new(Lifecycle::Uninitialized)),
            lifetime,
            pre_auth_manager,
            inspect,
        }
    }

    /// Asynchronously handles the supplied stream of `AccountHandlerControlRequest` messages.
    pub async fn handle_requests_from_stream(
        &self,
        mut stream: AccountHandlerControlRequestStream,
    ) -> Result<(), anyhow::Error> {
        while let Some(req) = stream.try_next().await? {
            self.handle_request(req).await?;
        }
        Ok(())
    }

    /// Dispatches an `AccountHandlerControlRequest` message to the appropriate handler method
    /// based on its type.
    pub async fn handle_request(
        &self,
        req: AccountHandlerControlRequest,
    ) -> Result<(), fidl::Error> {
        match req {
            AccountHandlerControlRequest::CreateAccount { payload, responder } => {
                let response = self
                    .create_account(payload.id, payload.interaction, payload.auth_mechanism_id)
                    .await;
                responder.send(&mut response.map(|state| state.into()))?;
            }
            AccountHandlerControlRequest::Preload { pre_auth_state, responder } => {
                let mut response = self.preload(AccountPreAuthState::from(pre_auth_state)).await;
                responder.send(&mut response)?;
            }
            AccountHandlerControlRequest::UnlockAccount { payload, responder } => {
                let response = self.unlock_account(payload.interaction).await;
                responder.send(&mut response.map(|opt| opt.map(|state| state.into())))?;
            }
            AccountHandlerControlRequest::LockAccount { responder } => {
                let response = self.lock_account().await;
                responder.send(&mut response.map(|opt| opt.map(|state| state.into())))?;
            }
            AccountHandlerControlRequest::RemoveAccount { responder } => {
                let mut response = self.remove_account().await;
                responder.send(&mut response)?;
            }
            AccountHandlerControlRequest::GetAccount { account, responder } => {
                let mut response = self.get_account(account).await;
                responder.send(&mut response)?;
            }
            AccountHandlerControlRequest::Terminate { control_handle } => {
                self.terminate().await;
                control_handle.shutdown();
            }
        }
        Ok(())
    }

    /// Connects to a specified authentication mechanism and return a proxy to it.
    async fn get_auth_mechanism_connection<'a>(
        &'a self,
        auth_mechanism_id: &'a str,
    ) -> Result<StorageUnlockMechanismProxy, ApiError> {
        if !DEV_AUTHENTICATION_MECHANISM_PATHS.contains_key(auth_mechanism_id) {
            warn!("Invalid auth mechanism id: {}", auth_mechanism_id);
            return Err(ApiError::InvalidRequest);
        }

        let auth_mechanism_proxy = client::connect_to_protocol_at_path::<
            StorageUnlockMechanismMarker,
        >(DEV_AUTHENTICATION_MECHANISM_PATHS[auth_mechanism_id])
        .map_err(|err| {
            warn!("Failed to connect to authenticator {:?}", err);
            ApiError::Resource
        })?;
        Ok(auth_mechanism_proxy)
    }

    /// Creates a new system account and attaches it to this handler.  Moves
    /// the handler from the `Uninitialized` to the `Initialized` state.
    // TODO(fxb/104199): Remove auth_mechanism once interaction is used for tests.
    async fn create_account(
        &self,
        _account_id: Option<FidlAccountId>,
        _interaction: Option<ServerEnd<InteractionMarker>>,
        auth_mechanism_id: Option<String>,
    ) -> Result<AccountPreAuthState, ApiError> {
        // TODO(fxb/104337): Implement the server end of the interaction protocol.
        let (_, test_interaction_server_end) = create_endpoints().unwrap();
        let mut test_ipse = InteractionProtocolServerEnd::Test(test_interaction_server_end);

        let mut state_lock = self.state.lock().await;
        match *state_lock {
            Lifecycle::Uninitialized => {
                let pre_auth_state = match (&self.lifetime, auth_mechanism_id) {
                    (AccountLifetime::Persistent { .. }, Some(auth_mechanism_id)) => {
                        let auth_mechanism_proxy =
                            self.get_auth_mechanism_connection(&auth_mechanism_id).await?;
                        let (data, prekey_material) = auth_mechanism_proxy
                            .enroll(&mut test_ipse)
                            .await
                            .map_err(|err| {
                                warn!("Error connecting to authenticator: {:?}", err);
                                ApiError::Unknown
                            })?
                            .map_err(|authenticator_err| {
                                warn!(
                                    "Error enrolling authentication mechanism: {:?}",
                                    authenticator_err
                                );
                                AccountManagerError::from(authenticator_err).api_error
                            })?;
                        // TODO(fxbug.dev/45041): Use storage manager for key validation
                        if prekey_material != *MAGIC_PREKEY {
                            warn!("Received unexpected pre-key material from authenticator");
                            return Err(ApiError::Internal);
                        }
                        pre_auth::State::SingleEnrollment { auth_mechanism_id, data }
                    }
                    (AccountLifetime::Ephemeral, Some(_)) => {
                        warn!(
                            "CreateAccount called with auth_mechanism_id set on an ephemeral \
                              account"
                        );
                        return Err(ApiError::InvalidRequest);
                    }
                    (_, None) => pre_auth::State::NoEnrollments,
                };
                self.pre_auth_manager.put(pre_auth_state).await.map_err(|err| {
                    warn!("Could not write pre-auth data: {:?}", err);
                    err.api_error
                })?;
                let sender = self.create_lock_request_sender().await.map_err(|err| {
                    warn!("Error constructing lock request sender: {:?}", err);
                    err.api_error
                })?;
                let account =
                    Account::create(self.lifetime.clone(), sender, self.inspect.get_node())
                        .await
                        .map_err(|err| err.api_error)?;
                *state_lock = Lifecycle::Initialized { account: Arc::new(account) };
                self.inspect.lifecycle.set("initialized");
                Ok(AccountPreAuthState {})
            }
            ref invalid_state @ _ => {
                warn!("CreateAccount was called in the {:?} state", invalid_state);
                Err(ApiError::FailedPrecondition)
            }
        }
    }

    /// Loads pre-authentication state for an account.  Moves the handler from
    /// the `Uninitialized` to the `Locked` state.
    async fn preload(&self, _pre_auth_state: AccountPreAuthState) -> Result<(), ApiError> {
        if self.lifetime == AccountLifetime::Ephemeral {
            warn!("Preload was called on an ephemeral account");
            return Err(ApiError::InvalidRequest);
        }
        let mut state_lock = self.state.lock().await;
        match *state_lock {
            Lifecycle::Uninitialized => {
                // Fetch the pre-auth state for validation, but ignore it.
                let _ = self.pre_auth_manager.get().await.map_err(|err| {
                    warn!("Error fetching pre-auth state: {:?}", err);
                    err.api_error
                })?;
                *state_lock = Lifecycle::Locked;
                self.inspect.lifecycle.set("locked");
                Ok(())
            }
            ref invalid_state @ _ => {
                warn!("Preload was called in the {:?} state", invalid_state);
                Err(ApiError::FailedPrecondition)
            }
        }
    }

    /// Unlocks an existing system account and attaches it to this handler.
    /// If the account is enrolled with an authentication mechanism,
    /// authentication will be performed as part of this call. Moves
    /// the handler to the `Initialized` state.
    async fn unlock_account(
        &self,
        interaction: Option<ServerEnd<InteractionMarker>>,
    ) -> Result<Option<AccountPreAuthState>, ApiError> {
        let mut state_lock = self.state.lock().await;
        match &*state_lock {
            Lifecycle::Initialized { .. } => {
                info!("UnlockAccount was called in the Initialized state, quietly succeeding.");
                return Ok(None);
            }
            Lifecycle::Locked => {
                let (prekey_material, updated_state) =
                    self.authenticate(interaction).await.map_err(|err| {
                        warn!("Authentication error: {:?}", err);
                        err.api_error
                    })?;
                // TODO(fxbug.dev/45041): Use storage manager for key validation
                if let Some(prekey_material) = prekey_material {
                    if prekey_material != *MAGIC_PREKEY {
                        info!("Encountered a failed authentication attempt");
                        return Err(ApiError::FailedAuthentication);
                    }
                }
                let sender = self.create_lock_request_sender().await.map_err(|err| {
                    warn!("Error constructing lock request sender: {:?}", err);
                    err.api_error
                })?;
                let account = Account::load(self.lifetime.clone(), sender, self.inspect.get_node())
                    .await
                    .map_err(|err| err.api_error)?;
                if let Some(updated_state) = updated_state {
                    self.pre_auth_manager.put(updated_state).await.map_err(|err| err.api_error)?;
                }
                *state_lock = Lifecycle::Initialized { account: Arc::new(account) };
                self.inspect.lifecycle.set("initialized");
                Ok(None)
            }
            ref invalid_state @ _ => {
                warn!("UnlockAccount was called in the {:?} state", invalid_state);
                Err(ApiError::FailedPrecondition)
            }
        }
    }

    /// Locks the account, terminating all open Account and Persona channels.  Moves
    /// the handler to the `Locked` state.
    async fn lock_account(&self) -> Result<Option<AccountPreAuthState>, ApiError> {
        Self::lock_now(Arc::clone(&self.state), Arc::clone(&self.inspect))
            .await
            .map_err(|err| {
                warn!("LockAccount call failed: {:?}", err);
                err.api_error
            })
            .map(|_| None)
    }

    /// Remove the active account. This method should not be retried on failure.
    async fn remove_account(&self) -> Result<(), ApiError> {
        let old_lifecycle = {
            let mut state_lock = self.state.lock().await;
            std::mem::replace(&mut *state_lock, Lifecycle::Finished)
        };
        self.inspect.lifecycle.set("finished");
        let account_arc = match old_lifecycle {
            Lifecycle::Locked { .. } => {
                warn!("Removing a locked account is not yet implemented");
                return Err(ApiError::UnsupportedOperation);
            }
            Lifecycle::Initialized { account, .. } => account,
            _ => {
                warn!("No account is initialized");
                return Err(ApiError::FailedPrecondition);
            }
        };
        // TODO(fxbug.dev/555): After this point, error recovery might include putting the account back
        // in the lock.
        if let Err(TaskGroupError::AlreadyCancelled) = account_arc.task_group().cancel().await {
            warn!("Task group was already cancelled prior to account removal.");
        }
        // At this point we have exclusive access to the account, so we move it out of the Arc to
        // destroy it.
        let account = Arc::try_unwrap(account_arc).map_err(|_| {
            warn!("Could not acquire exclusive access to account");
            ApiError::Internal
        })?;
        account.remove().map_err(|(_account, err)| {
            warn!("Could not delete account: {:?}", err);
            err.api_error
        })?;
        self.pre_auth_manager.remove().await.map_err(|err| {
            warn!("Could not remove pre-auth data: {:?}", err);
            err.api_error
        })?;
        info!("Deleted account");
        Ok(())
    }

    /// Connects the provided `account_server_end` to the `Account` protocol
    /// served by this handler.
    async fn get_account(
        &self,
        account_server_end: ServerEnd<AccountMarker>,
    ) -> Result<(), ApiError> {
        let account_arc = match &*self.state.lock().await {
            Lifecycle::Initialized { account, .. } => Arc::clone(account),
            _ => {
                warn!("AccountHandler is not initialized");
                return Err(ApiError::FailedPrecondition);
            }
        };

        let context = AccountContext {};
        let stream = account_server_end.into_stream().map_err(|err| {
            warn!("Error opening Account channel {:?}", err);
            ApiError::Resource
        })?;

        let account_arc_clone = Arc::clone(&account_arc);
        account_arc
            .task_group()
            .spawn(|cancel| async move {
                account_arc_clone
                    .handle_requests_from_stream(&context, stream, cancel)
                    .await
                    .unwrap_or_else(|e| error!("Error handling Account channel: {:?}", e));
            })
            .await
            .map_err(|_| {
                // Since AccountHandler serves only one channel of requests in serial, this is an
                // inconsistent state rather than a conflict
                ApiError::Internal
            })
    }

    async fn terminate(&self) {
        info!("Gracefully shutting down AccountHandler");
        let old_state = {
            let mut state_lock = self.state.lock().await;
            std::mem::replace(&mut *state_lock, Lifecycle::Finished)
        };
        if let Lifecycle::Initialized { account, .. } = old_state {
            if account.task_group().cancel().await.is_err() {
                warn!("Task group cancelled but account is still initialized");
            }
        }
    }

    /// Performs an authentication attempt if appropriate. Returns pre-key
    /// material from the attempt if the account is configured with a key, and
    /// optionally a new pre-authentication state, to be written if the
    /// attempt is successful.
    async fn authenticate(
        &self,
        _interaction: Option<ServerEnd<InteractionMarker>>,
    ) -> Result<(Option<Vec<u8>>, Option<pre_auth::State>), AccountManagerError> {
        // TODO(fxb/104337): Implement the server end of the supplied interaction protocol to
        // spawn new connections to authenticators.
        let (_, test_interaction_server_end) = create_endpoints().unwrap();
        let mut test_ipse = InteractionProtocolServerEnd::Test(test_interaction_server_end);

        let pre_auth_state = self.pre_auth_manager.get().await.map_err(|err| {
            warn!("Error fetching pre-auth state: {:?}", err);
            err.api_error
        })?;
        if let pre_auth::State::SingleEnrollment { ref auth_mechanism_id, ref data } =
            pre_auth_state.as_ref()
        {
            let auth_mechanism_proxy =
                self.get_auth_mechanism_connection(auth_mechanism_id).await?;
            let mut enrollments = vec![Enrollment { id: ENROLLMENT_ID, data: data.clone() }];
            let fut =
                auth_mechanism_proxy.authenticate(&mut test_ipse, &mut enrollments.iter_mut());
            let auth_attempt = fut.await.map_err(|err| {
                AccountManagerError::new(ApiError::Unknown)
                    .with_cause(format_err!("Error connecting to authenticator: {:?}", err))
            })??;
            match auth_attempt.enrollment_id {
                None => Err(AccountManagerError::new(ApiError::Internal).with_cause(format_err!(
                    "Authenticator returned an empty enrollment id during authentication."
                ))),
                Some(id) if id != ENROLLMENT_ID =>
                // TODO(dnordstrom): Error code for unexpected behavior from another component.
                {
                    Err(AccountManagerError::new(ApiError::Internal).with_cause(format_err!(
                    "Authenticator returned an unexpected enrollment id {} during authentication.",
                    id)))
                }
                _ => Ok(()),
            }?;
            // Determine whether pre-auth state should be updated
            let updated_pre_auth_state = auth_attempt.updated_enrollment_data.map(|data| {
                pre_auth::State::SingleEnrollment {
                    auth_mechanism_id: auth_mechanism_id.to_string(),
                    data: data,
                }
            });
            Ok((auth_attempt.prekey_material, updated_pre_auth_state))
        } else {
            Ok((None, None))
        }
    }

    /// Returns a sender which, when dispatched, causes the account handler
    /// to transition to the locked state. This method spawns
    /// a task monitoring the lock request (which terminates quitely if the
    /// sender is dropped). If lock requests are not supported for the account,
    /// depending on the pre-auth state, an unsupported lock request sender is
    /// returned.
    async fn create_lock_request_sender(
        &self,
    ) -> Result<lock_request::Sender, AccountManagerError> {
        // Lock requests are only supported for accounts with an enrolled
        // storage unlock mechanism
        if self.pre_auth_manager.get().await?.as_ref() == &pre_auth::State::NoEnrollments {
            return Ok(lock_request::Sender::NotSupported);
        }
        // Use weak pointers in order to not interfere with destruction of AccountHandler
        let state_weak = Arc::downgrade(&self.state);
        let inspect_weak = Arc::downgrade(&self.inspect);
        let (sender, receiver) = lock_request::channel();
        fasync::Task::spawn(async move {
            match receiver.await {
                Ok(()) => {
                    if let (Some(state), Some(inspect)) =
                        (state_weak.upgrade(), inspect_weak.upgrade())
                    {
                        if let Err(err) = Self::lock_now(state, inspect).await {
                            warn!("Lock request failure: {:?}", err);
                        }
                    }
                }
                Err(oneshot::Canceled) => {
                    // The sender was dropped, which is on the expected path.
                }
            }
        })
        .detach();
        Ok(sender)
    }

    /// Moves the provided lifecycle to the lock state, and notifies the inspect
    /// node of the change. Succeeds quitely if already locked.
    async fn lock_now(
        state: Arc<Mutex<Lifecycle>>,
        inspect: Arc<inspect::AccountHandler>,
    ) -> Result<(), AccountManagerError> {
        let mut state_lock = state.lock().await;
        match &*state_lock {
            Lifecycle::Locked { .. } => {
                info!("A lock operation was attempted in the locked state, quietly succeeding.");
                Ok(())
            }
            Lifecycle::Initialized { account } => {
                let _ = account.task_group().cancel().await; // Ignore AlreadyCancelled error
                let new_state = Lifecycle::Locked;
                *state_lock = new_state;
                inspect.lifecycle.set("locked");
                Ok(())
            }
            ref invalid_state @ _ => Err(AccountManagerError::new(ApiError::FailedPrecondition)
                .with_cause(format_err!(
                    "A lock operation was attempted in the {:?} state",
                    invalid_state
                ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;
    use fidl::endpoints::{create_endpoints, create_proxy_and_stream};
    use fidl_fuchsia_identity_internal::{
        AccountHandlerControlCreateAccountRequest, AccountHandlerControlMarker,
        AccountHandlerControlProxy, AccountHandlerControlUnlockAccountRequest,
    };
    use fuchsia_async as fasync;
    use fuchsia_async::DurationExt;
    use fuchsia_inspect::testing::AnyProperty;
    use fuchsia_inspect::{assert_data_tree, Inspector};
    use fuchsia_zircon as zx;
    use futures::future::join;
    use lazy_static::lazy_static;
    use std::sync::Arc;

    const TEST_AUTH_MECHANISM_ID: &str = "<AUTH MECHANISM ID>";

    lazy_static! {
        /// Initial enrollment data
        static ref TEST_ENROLLMENT_DATA: Vec<u8> = vec![13, 37];

        /// Updated enrollment data
        static ref TEST_UPDATED_ENROLLMENT_DATA: Vec<u8> = vec![14, 37];

        /// An initial pre-authentication state with a single enrollment
        static ref TEST_PRE_AUTH_SINGLE: pre_auth::State = pre_auth::State::SingleEnrollment {
            auth_mechanism_id: TEST_AUTH_MECHANISM_ID.to_string(),
            data: TEST_ENROLLMENT_DATA.clone(),
        };

        /// An updated pre-authentication state
        static ref TEST_PRE_AUTH_UPDATED: pre_auth::State = pre_auth::State::SingleEnrollment {
            auth_mechanism_id: TEST_AUTH_MECHANISM_ID.to_string(),
            data: TEST_UPDATED_ENROLLMENT_DATA.clone(),
        };

        /// Pre-key material that fails authentication.
        static ref TEST_NOT_MAGIC_PREKEY: Vec<u8>  = vec![80; 32];

        /// Assumed time between a lock request and when the account handler is locked
        static ref LOCK_REQUEST_DURATION: zx::Duration = zx::Duration::from_millis(20);
    }

    /// An enum expressing unexpected errors that may occur during a test.
    #[derive(Debug)]
    enum AccountHandlerTestError {
        FidlError(fidl::Error),
        AccountError(ApiError),
    }

    impl From<fidl::Error> for AccountHandlerTestError {
        fn from(fidl_error: fidl::Error) -> Self {
            AccountHandlerTestError::FidlError(fidl_error)
        }
    }

    impl From<ApiError> for AccountHandlerTestError {
        fn from(err: ApiError) -> Self {
            AccountHandlerTestError::AccountError(err)
        }
    }

    type TestResult = Result<(), AccountHandlerTestError>;

    fn create_account_handler(
        lifetime: AccountLifetime,
        pre_auth_manager: Arc<dyn pre_auth::Manager>,
        inspector: Arc<Inspector>,
    ) -> (AccountHandlerControlProxy, impl Future<Output = ()>) {
        let test_object = AccountHandler::new(
            TEST_ACCOUNT_ID.clone().into(),
            lifetime,
            pre_auth_manager,
            &inspector,
        );
        let (proxy, request_stream) = create_proxy_and_stream::<AccountHandlerControlMarker>()
            .expect("Failed to create proxy and stream");

        let server_fut = async move {
            test_object
                .handle_requests_from_stream(request_stream)
                .await
                .unwrap_or_else(|err| panic!("Fatal error handling test request: {:?}", err));

            // Check that no more objects are lurking in inspect
            std::mem::drop(test_object);
            assert_data_tree!(inspector, root: {});
        };

        (proxy, server_fut)
    }

    fn request_stream_test<TestFn, Fut>(
        lifetime: AccountLifetime,
        pre_auth_manager: Arc<dyn pre_auth::Manager>,
        inspector: Arc<Inspector>,
        test_fn: TestFn,
    ) where
        TestFn: FnOnce(AccountHandlerControlProxy) -> Fut,
        Fut: Future<Output = TestResult>,
    {
        let mut executor = fasync::LocalExecutor::new().expect("Failed to create executor");
        let (proxy, server_fut) = create_account_handler(lifetime, pre_auth_manager, inspector);

        let (test_res, _server_result) =
            executor.run_singlethreaded(join(test_fn(proxy), server_fut));

        assert!(test_res.is_ok());
    }

    fn create_clean_pre_auth_manager() -> Arc<dyn pre_auth::Manager> {
        Arc::new(pre_auth::InMemoryManager::create(pre_auth::State::NoEnrollments))
    }

    #[test]
    fn test_get_account_before_initialization() {
        let location = TempLocation::new();
        request_stream_test(
            location.to_persistent_lifetime(),
            create_clean_pre_auth_manager(),
            Arc::new(Inspector::new()),
            |proxy| async move {
                let (_, account_server_end) = create_endpoints().unwrap();
                assert_eq!(
                    proxy.get_account(account_server_end).await?,
                    Err(ApiError::FailedPrecondition)
                );
                Ok(())
            },
        );
    }

    #[test]
    fn test_get_account_when_locked() {
        let location = TempLocation::new();
        request_stream_test(
            location.to_persistent_lifetime(),
            create_clean_pre_auth_manager(),
            Arc::new(Inspector::new()),
            |proxy| async move {
                let (_, account_server_end) = create_endpoints().unwrap();
                let pre_auth_state: Vec<u8> = AccountPreAuthState {}.into();
                proxy.preload(&pre_auth_state).await??;
                assert_eq!(
                    proxy.get_account(account_server_end).await?,
                    Err(ApiError::FailedPrecondition)
                );
                Ok(())
            },
        );
    }

    #[test]
    fn test_double_initialize() {
        let location = TempLocation::new();
        request_stream_test(
            location.to_persistent_lifetime(),
            create_clean_pre_auth_manager(),
            Arc::new(Inspector::new()),
            |proxy| async move {
                proxy.create_account(AccountHandlerControlCreateAccountRequest::EMPTY).await??;

                assert_eq!(
                    proxy.create_account(AccountHandlerControlCreateAccountRequest::EMPTY).await?,
                    Err(ApiError::FailedPrecondition)
                );
                Ok(())
            },
        );
    }

    #[test]
    fn test_create_get_and_lock_account() {
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            location.to_persistent_lifetime(),
            create_clean_pre_auth_manager(),
            Arc::clone(&inspector),
            |account_handler_proxy| {
                async move {
                    account_handler_proxy
                        .create_account(AccountHandlerControlCreateAccountRequest::EMPTY)
                        .await??;
                    assert_data_tree!(inspector, root: {
                        account_handler: contains {
                            account: contains {
                                open_client_channels: 0u64,
                            },
                        }
                    });

                    let (account_client_end, account_server_end) = create_endpoints().unwrap();
                    account_handler_proxy.get_account(account_server_end).await??;

                    assert_data_tree!(inspector, root: {
                        account_handler: contains {
                            lifecycle: "initialized",
                            account: contains {
                                open_client_channels: 1u64,
                            },
                        }
                    });

                    // The account channel should now be usable.
                    let account_proxy = account_client_end.into_proxy().unwrap();
                    assert_eq!(
                        account_proxy.get_auth_state().await?,
                        Err(ApiError::UnsupportedOperation)
                    );

                    // Lock the account and check that channels are closed
                    account_handler_proxy.lock_account().await??;
                    assert_data_tree!(inspector, root: {
                        account_handler: contains {
                            lifecycle: "locked",
                        }
                    });
                    assert!(account_proxy.get_auth_state().await.is_err());
                    Ok(())
                }
            },
        );
    }

    #[test]
    fn test_preload_and_unlock_existing_account() {
        // Create an account
        let location = TempLocation::new();
        let pre_auth_manager = create_clean_pre_auth_manager();
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            location.to_persistent_lifetime(),
            Arc::clone(&pre_auth_manager),
            Arc::clone(&inspector),
            |proxy| async move {
                proxy.create_account(AccountHandlerControlCreateAccountRequest::EMPTY).await??;
                assert_data_tree!(inspector, root: {
                    account_handler: contains {
                        lifecycle: "initialized",
                    }
                });
                Ok(())
            },
        );

        // Ensure the account is persisted by unlocking it
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            location.to_persistent_lifetime(),
            pre_auth_manager,
            Arc::clone(&inspector),
            |proxy| async move {
                let pre_auth_state: Vec<u8> = AccountPreAuthState {}.into();
                proxy.preload(&pre_auth_state).await??;
                assert_data_tree!(inspector, root: {
                    account_handler: contains {
                        lifecycle: "locked",
                    }
                });
                proxy.unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY).await??;
                assert_data_tree!(inspector, root: {
                    account_handler: contains {
                        lifecycle: "initialized",
                    }
                });
                Ok(())
            },
        );
    }

    #[test]
    fn test_multiple_unlocks() {
        // Create an account
        let location = TempLocation::new();
        let pre_auth_manager = create_clean_pre_auth_manager();
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            location.to_persistent_lifetime(),
            Arc::clone(&pre_auth_manager),
            Arc::clone(&inspector),
            |proxy| async move {
                proxy.create_account(AccountHandlerControlCreateAccountRequest::EMPTY).await??;
                proxy.lock_account().await??;
                proxy.unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY).await??;
                proxy.lock_account().await??;
                proxy.unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY).await??;
                Ok(())
            },
        );
    }

    #[test]
    fn test_unlock_uninitialized_account() {
        let location = TempLocation::new();
        let pre_auth_manager =
            Arc::new(pre_auth::InMemoryManager::create(TEST_PRE_AUTH_SINGLE.clone()));
        request_stream_test(
            location.to_persistent_lifetime(),
            pre_auth_manager,
            Arc::new(Inspector::new()),
            |proxy| async move {
                assert_eq!(
                    proxy.unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY).await?,
                    Err(ApiError::FailedPrecondition)
                );
                Ok(())
            },
        );
    }

    #[test]
    fn test_remove_account() {
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            location.to_persistent_lifetime(),
            create_clean_pre_auth_manager(),
            Arc::clone(&inspector),
            |proxy| {
                async move {
                    assert_data_tree!(inspector, root: {
                        account_handler: {
                            account_id: TEST_ACCOUNT_ID_UINT,
                            lifecycle: "uninitialized",
                        }
                    });

                    proxy
                        .create_account(AccountHandlerControlCreateAccountRequest::EMPTY)
                        .await??;
                    assert_data_tree!(inspector, root: {
                        account_handler: {
                            account_id: TEST_ACCOUNT_ID_UINT,
                            lifecycle: "initialized",
                            account: {
                                open_client_channels: 0u64,
                            },
                            default_persona: {
                                persona_id: AnyProperty,
                                open_client_channels: 0u64,
                            },
                        }
                    });

                    // Keep an open channel to an account.
                    let (account_client_end, account_server_end) = create_endpoints().unwrap();
                    proxy.get_account(account_server_end).await??;
                    let account_proxy = account_client_end.into_proxy().unwrap();

                    // Make sure remove_account() can make progress with an open channel.
                    proxy.remove_account().await??;

                    assert_data_tree!(inspector, root: {
                        account_handler: {
                            account_id: TEST_ACCOUNT_ID_UINT,
                            lifecycle: "finished",
                        }
                    });

                    // Make sure that the channel is in fact closed.
                    assert!(account_proxy.get_auth_state().await.is_err());

                    // We cannot remove twice.
                    assert_eq!(proxy.remove_account().await?, Err(ApiError::FailedPrecondition));
                    Ok(())
                }
            },
        );
    }

    #[test]
    fn test_remove_locked_account() {
        let location = TempLocation::new();
        request_stream_test(
            location.to_persistent_lifetime(),
            create_clean_pre_auth_manager(),
            Arc::new(Inspector::new()),
            |proxy| async move {
                let pre_auth_state: Vec<u8> = AccountPreAuthState {}.into();
                // Preloading a non-existing account will succeed, for now
                proxy.preload(&pre_auth_state).await??;
                assert_eq!(proxy.remove_account().await?, Err(ApiError::UnsupportedOperation));
                Ok(())
            },
        );
    }

    #[test]
    fn test_remove_account_before_initialization() {
        let location = TempLocation::new();
        request_stream_test(
            location.to_persistent_lifetime(),
            create_clean_pre_auth_manager(),
            Arc::new(Inspector::new()),
            |proxy| async move {
                assert_eq!(proxy.remove_account().await?, Err(ApiError::FailedPrecondition));
                Ok(())
            },
        );
    }

    #[test]
    fn test_terminate() {
        let location = TempLocation::new();
        request_stream_test(
            location.to_persistent_lifetime(),
            create_clean_pre_auth_manager(),
            Arc::new(Inspector::new()),
            |proxy| {
                async move {
                    proxy
                        .create_account(AccountHandlerControlCreateAccountRequest::EMPTY)
                        .await??;

                    // Keep an open channel to an account.
                    let (account_client_end, account_server_end) = create_endpoints().unwrap();
                    proxy.get_account(account_server_end).await??;
                    let account_proxy = account_client_end.into_proxy().unwrap();

                    // Terminate the handler
                    proxy.terminate()?;

                    // Check that further operations fail
                    assert!(proxy.remove_account().await.is_err());
                    assert!(proxy.terminate().is_err());

                    // Make sure that the channel closed too.
                    assert!(account_proxy.get_auth_state().await.is_err());
                    Ok(())
                }
            },
        );
    }

    #[test]
    fn test_terminate_locked_account() {
        let location = TempLocation::new();
        request_stream_test(
            location.to_persistent_lifetime(),
            create_clean_pre_auth_manager(),
            Arc::new(Inspector::new()),
            |proxy| {
                async move {
                    proxy
                        .create_account(AccountHandlerControlCreateAccountRequest::EMPTY)
                        .await??;
                    proxy.lock_account().await??;
                    proxy.terminate()?;

                    // Check that further operations fail
                    assert!(proxy
                        .unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY)
                        .await
                        .is_err());
                    assert!(proxy.terminate().is_err());
                    Ok(())
                }
            },
        );
    }

    #[test]
    fn test_load_non_existing_account() {
        let location = TempLocation::new();
        request_stream_test(
            location.to_persistent_lifetime(),
            create_clean_pre_auth_manager(),
            Arc::new(Inspector::new()),
            |proxy| async move {
                let pre_auth_state: Vec<u8> = AccountPreAuthState {}.into();
                // Preloading a non-existing account will succeed, for now
                proxy.preload(&pre_auth_state).await??;
                assert_eq!(
                    proxy.unlock_account(AccountHandlerControlUnlockAccountRequest::EMPTY).await?,
                    Err(ApiError::NotFound)
                );
                Ok(())
            },
        );
    }

    #[test]
    fn test_create_account_ephemeral_with_auth_mechanism() {
        request_stream_test(
            AccountLifetime::Ephemeral,
            create_clean_pre_auth_manager(),
            Arc::new(Inspector::new()),
            |proxy| async move {
                assert_eq!(
                    proxy
                        .create_account(AccountHandlerControlCreateAccountRequest {
                            auth_mechanism_id: Some(TEST_AUTH_MECHANISM_ID.to_string()),
                            ..AccountHandlerControlCreateAccountRequest::EMPTY
                        })
                        .await?,
                    Err(ApiError::InvalidRequest)
                );
                Ok(())
            },
        );
    }

    #[test]
    fn test_lock_request_ephemeral_account_failure() {
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            AccountLifetime::Ephemeral,
            create_clean_pre_auth_manager(),
            Arc::clone(&inspector),
            |account_handler_proxy| async move {
                account_handler_proxy
                    .create_account(AccountHandlerControlCreateAccountRequest::EMPTY)
                    .await??;

                // Get a proxy to the Account interface
                let (account_client_end, account_server_end) = create_endpoints().unwrap();
                account_handler_proxy.get_account(account_server_end).await??;
                let account_proxy = account_client_end.into_proxy().unwrap();

                // Send the lock request
                assert_eq!(account_proxy.lock().await?, Err(ApiError::FailedPrecondition));

                // Wait for a potentitially faulty lock request to propagate
                fasync::Timer::new(LOCK_REQUEST_DURATION.clone().after_now()).await;

                // The channel is still usable
                assert!(account_proxy.get_persona_ids().await.is_ok());

                // The state remains initialized
                assert_data_tree!(inspector, root: {
                    account_handler: contains {
                        lifecycle: "initialized",
                    }
                });
                Ok(())
            },
        );
    }

    #[test]
    fn test_lock_request_persistent_account_without_auth_mechanism() {
        let location = TempLocation::new();
        let inspector = Arc::new(Inspector::new());
        request_stream_test(
            location.to_persistent_lifetime(),
            create_clean_pre_auth_manager(),
            Arc::clone(&inspector),
            |account_handler_proxy| async move {
                account_handler_proxy
                    .create_account(AccountHandlerControlCreateAccountRequest::EMPTY)
                    .await??;

                // Get a proxy to the Account interface
                let (account_client_end, account_server_end) = create_endpoints().unwrap();
                account_handler_proxy.get_account(account_server_end).await??;
                let account_proxy = account_client_end.into_proxy().unwrap();

                // Send the lock request
                assert_eq!(account_proxy.lock().await?, Err(ApiError::FailedPrecondition));
                Ok(())
            },
        );
    }
}
