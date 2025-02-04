//! Crate `ruma_client` is a [Matrix](https://matrix.org/) client library.
//!
//! # Usage
//!
//! Begin by creating a `Client` type, usually using the `https` method for a client that supports
//! secure connections, and then logging in:
//!
//! ```no_run
//! use futures::Future;
//! use ruma_client::Client;
//!
//! let homeserver_url = "https://example.com".parse().unwrap();
//! let client = Client::https(homeserver_url, None).unwrap();
//!
//! let work = client
//!     .log_in("@alice:example.com".to_string(), "secret".to_string(), None)
//!     .and_then(|session| {
//!         // You're now logged in! Write the session to a file if you want to restore it later.
//!         // Then start using the API!
//!         # Ok::<(), ruma_client::Error>(())
//!     });
//!
//! // Start `work` on a futures runtime...
//! ```
//!
//! You can also pass an existing session to the `Client` constructor to restore a previous session
//! rather than calling `log_in`.
//!
//! For the standard use case of synchronizing with the homeserver (i.e. getting all the latest
//! events), use the `Client::sync`:
//!
//! ```no_run
//! # use futures::{Future, Stream};
//! # use ruma_client::Client;
//! # let homeserver_url = "https://example.com".parse().unwrap();
//! # let client = Client::https(homeserver_url, None).unwrap();
//! let work = client.sync(None, None, true).map(|response| {
//!   // Do something with the data in the response...
//!     # Ok::<(), ruma_client::Error>(())
//! });
//!
//! // Start `work` on a futures runtime...
//! ```
//!
//! The `Client` type also provides methods for registering a new account if you don't already have
//! one with the given homeserver.
//!
//! Beyond these basic convenience methods, `ruma-client` gives you access to the entire Matrix
//! client-server API via the `api` module. Each leaf module under this tree of modules contains
//! the necessary types for one API endpoint. Simply call the module's `call` method, passing it
//! the logged in `Client` and the relevant `Request` type. `call` will return a future that will
//! resolve to the relevant `Response` type.
//!
//! For example:
//!
//! ```no_run
//! # use futures::Future;
//! # use ruma_client::Client;
//! # let homeserver_url = "https://example.com".parse().unwrap();
//! # let client = Client::https(homeserver_url, None).unwrap();
//! use std::convert::TryFrom;
//!
//! use ruma_client::api::r0::alias::get_alias;
//! use ruma_identifiers::{RoomAliasId, RoomId};
//!
//! let request = get_alias::Request {
//!     room_alias: RoomAliasId::try_from("#example_room:example.com").unwrap(),
//! };
//!
//! let work = get_alias::call(client, request).and_then(|response| {
//!     assert_eq!(response.room_id, RoomId::try_from("!n8f893n9:example.com").unwrap());
//!     # Ok::<(), ruma_client::Error>(())
//! });
//!
//! // Start `work` on a futures runtime...
//! ```

#![deny(
    missing_copy_implementations,
    missing_debug_implementations,
    missing_docs,
    warnings
)]
#![warn(
    clippy::empty_line_after_outer_attr,
    clippy::expl_impl_clone_on_copy,
    clippy::if_not_else,
    clippy::items_after_statements,
    clippy::match_same_arms,
    clippy::mem_forget,
    clippy::missing_docs_in_private_items,
    clippy::mut_mut,
    clippy::needless_borrow,
    clippy::needless_continue,
    clippy::single_match_else,
    clippy::unicode_not_nfc,
    clippy::use_self,
    clippy::used_underscore_binding,
    clippy::wrong_pub_self_convention,
    clippy::wrong_self_convention
)]

use std::{
    convert::TryInto,
    str::FromStr,
    sync::{Arc, Mutex},
};

use futures::{
    future::{Future, FutureFrom, IntoFuture},
    stream::{self, Stream},
};
use hyper::{
    client::{connect::Connect, HttpConnector},
    Client as HyperClient, Uri,
};
#[cfg(feature = "hyper-tls")]
use hyper_tls::HttpsConnector;
#[cfg(feature = "hyper-tls")]
use native_tls::Error as NativeTlsError;
use ruma_api::Endpoint;
use url::Url;

use crate::error::InnerError;
pub use crate::{error::Error, session::Session};

/// Matrix client-server API endpoints.
pub mod api;
mod error;
mod session;

/// A client for the Matrix client-server API.
#[derive(Debug)]
pub struct Client<C: Connect>(Arc<ClientData<C>>);

/// Data contained in Client's Rc
#[derive(Debug)]
struct ClientData<C>
where
    C: Connect,
{
    /// The URL of the homeserver to connect to.
    homeserver_url: Url,
    /// The underlying HTTP client.
    hyper: HyperClient<C>,
    /// User session data.
    session: Mutex<Option<Session>>,
}

impl Client<HttpConnector> {
    /// Creates a new client for making HTTP requests to the given homeserver.
    pub fn new(homeserver_url: Url, session: Option<Session>) -> Self {
        Self(Arc::new(ClientData {
            homeserver_url,
            hyper: HyperClient::builder().keep_alive(true).build_http(),
            session: Mutex::new(session),
        }))
    }

    /// Get a copy of the current `Session`, if any.
    ///
    /// Useful for serializing and persisting the session to be restored later.
    pub fn session(&self) -> Option<Session> {
        self.0
            .session
            .lock()
            .expect("session mutex was poisoned")
            .clone()
    }
}

#[cfg(feature = "tls")]
impl Client<HttpsConnector<HttpConnector>> {
    /// Creates a new client for making HTTPS requests to the given homeserver.
    pub fn https(homeserver_url: Url, session: Option<Session>) -> Result<Self, NativeTlsError> {
        let connector = HttpsConnector::new(4)?;

        Ok(Self(Arc::new(ClientData {
            homeserver_url,
            hyper: { HyperClient::builder().keep_alive(true).build(connector) },
            session: Mutex::new(session),
        })))
    }
}

impl<C> Client<C>
where
    C: Connect + 'static,
{
    /// Creates a new client using the given `hyper::Client`.
    ///
    /// This allows the user to configure the details of HTTP as desired.
    pub fn custom(
        hyper_client: HyperClient<C>,
        homeserver_url: Url,
        session: Option<Session>,
    ) -> Self {
        Self(Arc::new(ClientData {
            homeserver_url,
            hyper: hyper_client,
            session: Mutex::new(session),
        }))
    }

    /// Log in with a username and password.
    ///
    /// In contrast to api::r0::session::login::call(), this method stores the
    /// session data returned by the endpoint in this client, instead of
    /// returning it.
    pub fn log_in(
        &self,
        user: String,
        password: String,
        device_id: Option<String>,
    ) -> impl Future<Item = Session, Error = Error> {
        use crate::api::r0::session::login;

        let data = self.0.clone();

        login::call(
            self.clone(),
            login::Request {
                address: None,
                login_type: login::LoginType::Password,
                medium: None,
                device_id,
                password,
                user,
            },
        )
        .map(move |response| {
            let session = Session {
                access_token: response.access_token,
                device_id: response.device_id,
                user_id: response.user_id,
            };
            *data.session.lock().unwrap() = Some(session.clone());

            session
        })
    }

    /// Register as a guest. In contrast to api::r0::account::register::call(),
    /// this method stores the session data returned by the endpoint in this
    /// client, instead of returning it.
    pub fn register_guest(&self) -> impl Future<Item = Session, Error = Error> {
        use crate::api::r0::account::register;

        let data = self.0.clone();

        register::call(
            self.clone(),
            register::Request {
                auth: None,
                bind_email: None,
                device_id: None,
                initial_device_display_name: None,
                kind: Some(register::RegistrationKind::Guest),
                password: None,
                username: None,
            },
        )
        .map(move |response| {
            let session = Session {
                access_token: response.access_token,
                device_id: response.device_id,
                user_id: response.user_id,
            };
            *data.session.lock().unwrap() = Some(session.clone());

            session
        })
    }

    /// Register as a new user on this server.
    ///
    /// In contrast to api::r0::account::register::call(), this method stores
    /// the session data returned by the endpoint in this client, instead of
    /// returning it.
    ///
    /// The username is the local part of the returned user_id. If it is
    /// omitted from this request, the server will generate one.
    pub fn register_user(
        &self,
        username: Option<String>,
        password: String,
    ) -> impl Future<Item = Session, Error = Error> {
        use crate::api::r0::account::register;

        let data = self.0.clone();

        register::call(
            self.clone(),
            register::Request {
                auth: None,
                bind_email: None,
                device_id: None,
                initial_device_display_name: None,
                kind: Some(register::RegistrationKind::User),
                password: Some(password),
                username,
            },
        )
        .map(move |response| {
            let session = Session {
                access_token: response.access_token,
                device_id: response.device_id,
                user_id: response.user_id,
            };
            *data.session.lock().unwrap() = Some(session.clone());

            session
        })
    }

    /// Convenience method that represents repeated calls to the sync_events endpoint as a stream.
    ///
    /// If the since parameter is None, the first Item might take a significant time to arrive and
    /// be deserialized, because it contains all events that have occured in the whole lifetime of
    /// the logged-in users account and are visible to them.
    pub fn sync(
        &self,
        filter: Option<api::r0::sync::sync_events::Filter>,
        since: Option<String>,
        set_presence: bool,
    ) -> impl Stream<Item = api::r0::sync::sync_events::Response, Error = Error> {
        use crate::api::r0::sync::sync_events;

        let client = self.clone();
        let set_presence = if set_presence {
            None
        } else {
            Some(sync_events::SetPresence::Offline)
        };

        stream::unfold(since, move |since| {
            Some(
                sync_events::call(
                    client.clone(),
                    sync_events::Request {
                        filter: filter.clone(),
                        since,
                        full_state: None,
                        set_presence: set_presence.clone(),
                        timeout: None,
                    },
                )
                .map(|res| {
                    let next_batch_clone = res.next_batch.clone();
                    (res, Some(next_batch_clone))
                }),
            )
        })
    }

    /// Makes a request to a Matrix API endpoint.
    pub(crate) fn request<E>(
        self,
        request: <E as Endpoint>::Request,
    ) -> impl Future<Item = E::Response, Error = Error>
    where
        E: Endpoint,
    {
        let data1 = self.0.clone();
        let data2 = self.0.clone();
        let mut url = self.0.homeserver_url.clone();

        request
            .try_into()
            .map_err(Error::from)
            .into_future()
            .and_then(move |hyper_request| {
                {
                    let uri = hyper_request.uri();

                    url.set_path(uri.path());
                    url.set_query(uri.query());

                    if E::METADATA.requires_authentication {
                        if let Some(ref session) = *data1.session.lock().unwrap() {
                            url.query_pairs_mut()
                                .append_pair("access_token", &session.access_token);
                        } else {
                            return Err(Error(InnerError::AuthenticationRequired));
                        }
                    }
                }

                Uri::from_str(url.as_ref())
                    .map(move |uri| (uri, hyper_request))
                    .map_err(Error::from)
            })
            .and_then(move |(uri, mut hyper_request)| {
                *hyper_request.uri_mut() = uri;

                data2.hyper.request(hyper_request).map_err(Error::from)
            })
            .and_then(|hyper_response| {
                E::Response::future_from(hyper_response).map_err(Error::from)
            })
    }
}

impl<C: Connect> Clone for Client<C> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
