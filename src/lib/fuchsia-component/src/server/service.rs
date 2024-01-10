// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The `Service` trait and its trait-object wrappers.

use {
    fidl::endpoints::{DiscoverableProtocolMarker, RequestStream, ServerEnd, ServiceRequest},
    fuchsia_async as fasync, fuchsia_zircon as zx,
    std::marker::PhantomData,
};

/// `Service` connects channels to service instances.
///
/// Note that this trait is implemented by the `FidlService` type.
pub trait Service {
    /// The type of the value yielded by the `spawn_service` callback.
    type Output;

    /// Create a new instance of the service on the provided `zx::Channel`.
    ///
    /// The value returned by this function will be yielded from the stream
    /// output of the `ServiceFs` type.
    fn connect(&mut self, channel: zx::Channel) -> Option<Self::Output>;
}

impl<F, O> Service for F
where
    F: FnMut(zx::Channel) -> Option<O>,
{
    type Output = O;
    fn connect(&mut self, channel: zx::Channel) -> Option<Self::Output> {
        (self)(channel)
    }
}

/// A wrapper for functions from `RequestStream` to `Output` which implements
/// `Service`.
///
/// This type throws away channels that cannot be converted to the appropriate
/// `RequestStream` type.
pub struct FidlService<F, RS, Output>
where
    F: FnMut(RS) -> Output,
    RS: RequestStream,
{
    f: F,
    marker: PhantomData<(RS, Output)>,
}

impl<F, RS, Output> From<F> for FidlService<F, RS, Output>
where
    F: FnMut(RS) -> Output,
    RS: RequestStream,
{
    fn from(f: F) -> Self {
        Self { f, marker: PhantomData }
    }
}

impl<F, RS, Output> Service for FidlService<F, RS, Output>
where
    F: FnMut(RS) -> Output,
    RS: RequestStream,
{
    type Output = Output;
    fn connect(&mut self, channel: zx::Channel) -> Option<Self::Output> {
        let chan = fasync::Channel::from_channel(channel);
        Some((self.f)(RS::from_channel(chan)))
    }
}

/// A wrapper for functions from `ServerEnd` to `Output` which implements
/// `Service`.
pub struct FidlServiceServerConnector<F, P, Output>
where
    F: FnMut(ServerEnd<P>) -> Output,
    P: DiscoverableProtocolMarker,
{
    f: F,
    marker: PhantomData<(P, Output)>,
}

impl<F, P, Output> From<F> for FidlServiceServerConnector<F, P, Output>
where
    F: FnMut(ServerEnd<P>) -> Output,
    P: DiscoverableProtocolMarker,
{
    fn from(f: F) -> Self {
        Self { f, marker: PhantomData }
    }
}

impl<F, P, Output> Service for FidlServiceServerConnector<F, P, Output>
where
    F: FnMut(ServerEnd<P>) -> Output,
    P: DiscoverableProtocolMarker,
{
    type Output = Output;
    fn connect(&mut self, channel: zx::Channel) -> Option<Self::Output> {
        let FidlServiceServerConnector { f, marker: _ } = self;
        Some(f(ServerEnd::new(channel)))
    }
}

/// A wrapper for functions from `ServiceRequest` to `Output` which implements
/// `Service`.
///
/// This type throws away channels that cannot be converted to the appropriate
/// `ServiceRequest` type.
pub struct FidlServiceMember<F, SR, Output>
where
    F: FnMut(SR) -> Output,
    SR: ServiceRequest,
{
    f: F,
    member: &'static str,
    marker: PhantomData<(SR, Output)>,
}

impl<F, SR, Output> FidlServiceMember<F, SR, Output>
where
    F: FnMut(SR) -> Output,
    SR: ServiceRequest,
{
    /// Creates an object that handles connections to a service member.
    pub fn new(f: F, member: &'static str) -> Self {
        Self { f, member, marker: PhantomData }
    }
}

impl<F, SR, Output> Service for FidlServiceMember<F, SR, Output>
where
    F: FnMut(SR) -> Output,
    SR: ServiceRequest,
{
    type Output = Output;

    fn connect(&mut self, channel: zx::Channel) -> Option<Self::Output> {
        let chan = fasync::Channel::from_channel(channel);
        Some((self.f)(SR::dispatch(self.member, chan)))
    }
}

/// A `!Send` (non-thread-safe) trait object encapsulating a `Service` with
/// the given `Output` type.
///
/// Types which implement the `Service` trait can be converted to objects of
/// this type via the `From`/`Into` traits.
pub struct ServiceObjLocal<'a, Output>(Box<dyn Service<Output = Output> + 'a>);

impl<'a, S: Service + 'a> From<S> for ServiceObjLocal<'a, S::Output> {
    fn from(service: S) -> Self {
        ServiceObjLocal(Box::new(service))
    }
}

/// A thread-safe (`Send`) trait object encapsulating a `Service` with
/// the given `Output` type.
///
/// Types which implement the `Service` trait and the `Send` trait can
/// be converted to objects of this type via the `From`/`Into` traits.
pub struct ServiceObj<'a, Output>(Box<dyn Service<Output = Output> + Send + 'a>);

impl<'a, S: Service + Send + 'a> From<S> for ServiceObj<'a, S::Output> {
    fn from(service: S) -> Self {
        ServiceObj(Box::new(service))
    }
}

/// A trait implemented by both `ServiceObj` and `ServiceObjLocal` that
/// allows code to be generic over thread-safety.
///
/// Code that uses `ServiceObj` will require `Send` bounds but will be
/// multithreaded-capable, while code that uses `ServiceObjLocal` will
/// allow non-`Send` types but will be restricted to singlethreaded
/// executors.
pub trait ServiceObjTrait {
    /// The output type of the underlying `Service`.
    type Output;

    /// Get a mutable reference to the underlying `Service` trait object.
    fn service(&mut self) -> &mut dyn Service<Output = Self::Output>;
}

impl<'a, Output> ServiceObjTrait for ServiceObjLocal<'a, Output> {
    type Output = Output;

    fn service(&mut self) -> &mut dyn Service<Output = Self::Output> {
        &mut *self.0
    }
}

impl<'a, Output> ServiceObjTrait for ServiceObj<'a, Output> {
    type Output = Output;

    fn service(&mut self) -> &mut dyn Service<Output = Self::Output> {
        &mut *self.0
    }
}
