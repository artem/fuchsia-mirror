// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_FIDL_CPP_INCLUDE_LIB_FIDL_CPP_UNIFIED_MESSAGING_DECLARATIONS_H_
#define SRC_LIB_FIDL_CPP_INCLUDE_LIB_FIDL_CPP_UNIFIED_MESSAGING_DECLARATIONS_H_

#include <lib/fidl/cpp/wire/wire_messaging_declarations.h>

#include <cstdint>

// This header centralizes the forward declarations for the various types in the
// unified messaging layer. The C++ FIDL code generator populates the concrete
// definitions from FIDL protocols in the schema.
namespace fidl {

// |Response| represents the response of a FIDL method call, using natural
// types. See |WireResponse| for the equivalent using wire types.
//
// When |Method| response has a payload, |Response| inherits from:
//
// - If |Method| uses the error syntax:
//     - If the success value is empty: `fit::result<AppError>`.
//     - Otherwise: `fit::result<AppError, SuccessValue>`.
// - If |Method| does not use the error syntax: the payload type.
//
// When |Method| response has no payload, those operations will be absent.
//
// When |Method| has no response (one-way), this class will be undefined.
template <typename Method>
class Response;

// |Result| represents the result of calling a two-way FIDL method |Method|.
//
// It inherits from different `fit::result` types depending on |Method|:
//
// - When the method does not use the error syntax:
//     - When the method response has no body:
//
//           fit::result<fidl::Error>
//
//     - When the method response has a body:
//
//           fit::result<fidl::Error, MethodPayload>
//
//       where `fidl::Error` is a type representing any transport error or
//       protocol level terminal errors such as epitaphs, and |MethodPayload|
//       is the response type.
//
// - When the method uses the error syntax:
//     - When the method response payload is an empty struct:
//
//           fit::result<fidl::ErrorsIn<Method>>
//
//     - When the method response payload is not an empty struct:
//
//           fit::result<fidl::ErrorsIn<Method>, MethodPayload>
//
//       where |MethodPayload| is the success type.
//
// See also |fidl::ErrorsIn|.
template <typename Method>
class Result;

namespace internal {

// |NaturalMethodTypes| contains information about an interaction:
//
// When |FidlMethod| has a request (including events) with a body:
// - Request: the natural domain object representing the request body.
//
// When |FidlMethod| has a response with a body:
// - Response: the natural domain object representing the response body.
//
// When |FidlMethod| is two-way:
// - Completer:      the completer type used on servers.
// - ResultCallback: the callback type for the corresponding asynchronous call.
template <typename FidlMethod>
struct NaturalMethodTypes;

// |NaturalWeakEventSender| borrows the server endpoint from a binding object and
// exposes methods for sending events with natural types.
template <typename FidlProtocol>
class NaturalWeakEventSender;

// |NaturalEventSender| borrows a server endpoint and exposes methods for sending
// events with natural types.
template <typename FidlProtocol>
class NaturalEventSender;

// |NaturalSyncClientImpl| implements methods for making synchronous
// FIDL calls with natural types.
//
// All specializations of |NaturalSyncClientImpl| should inherit from
// |fidl::internal::SyncEndpointManagedVeneer|.
template <typename Protocol>
class NaturalSyncClientImpl;

// |NaturalClientImpl| implements methods for making asynchronous FIDL calls
// with natural types.
//
// All specializations of |NaturalClientImpl| should inherit from
// |fidl::internal::NaturalClientBase|.
template <typename Protocol>
class NaturalClientImpl;

// |NaturalEventHandlerInterface| contains handlers for each event inside
// the protocol |FidlProtocol|.
template <typename FidlProtocol>
class NaturalEventHandlerInterface;

template <typename FidlProtocol>
class NaturalEventDispatcher;

// |NaturalServerDispatcher| is a helper type that decodes an incoming message
// and invokes the corresponding handler in the server implementation.
template <typename FidlProtocol>
struct NaturalServerDispatcher;

template <typename FidlMethod>
class NaturalCompleterBase;

}  // namespace internal

// |SyncEventHandler| is used by synchronous clients to handle events using
// natural types.
template <typename Protocol>
class SyncEventHandler;

// |AsyncEventHandler| is used by asynchronous clients to handle events using
// natural types. It also adds a callback for handling fatal errors.
template <typename Protocol>
class AsyncEventHandler;

// |Server| is a pure-virtual interface to be implemented by a server, receiving
// natural types.
template <typename Protocol>
class Server;

namespace testing {

// |TestBase<P>| is a in subclass of |fidl::Server<P>| with default implementations of all of the
// methods.
template <typename Protocol>
class TestBase;
}  // namespace testing

}  // namespace fidl

#endif  // SRC_LIB_FIDL_CPP_INCLUDE_LIB_FIDL_CPP_UNIFIED_MESSAGING_DECLARATIONS_H_
