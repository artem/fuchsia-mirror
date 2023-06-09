# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Module for handling encoding and decoding FIDL messages, as well as for handling async I/O."""
import asyncio
import inspect
import os
import sys
import typing

from fidl_codec import decode_fidl_response
from fidl_codec import encode_fidl_message
import fuchsia_controller_py as fc

TXID: int = 0
LOOP_STARTED = False
HANDLE_READY_QUEUES: typing.Dict[int, asyncio.Queue] = {}


def enqueue_ready_zx_handle_from_fd(
        fd: int, handle_ready_queues: typing.Dict[int, asyncio.Queue]):
    """Reads zx_handle that is ready for reading, and enqueues it in the appropriate ready queue."""
    handle_no = int.from_bytes(os.read(fd, 4), sys.byteorder)
    loop = asyncio.get_running_loop()
    queue = handle_ready_queues.get(handle_no)
    if queue:
        loop.create_task(queue.put(handle_no))


def make_default_obj(object_ty):
    """Takes a type `object_ty` and creates the default __init__ implementation of the object.

    Args:
        object_ty: The type of object which is being constructed (this is also the type of the
        return value).

    Returns:
        The default (all fields None) object created from object_ty.

    For example, if the object is a struct, it will return the "default" version of the struct,
    where all fields are set to None, regardless what the field type is.
    """
    sig = inspect.signature(object_ty.__init__)
    args = {}
    for arg in sig.parameters:
        if str(arg) == "self":
            continue
        args[str(arg)] = None
    if not args:
        return object_ty()
    try:
        return object_ty(**args)
    except TypeError:
        # Object might accept *args/**kwargs, so use empty constructor.
        return object_ty()


def get_type_from_import(i):
    """Takes an import and returns the Python type.

    Args:
        i: The python FIDL import string, e.g. "fidl.foo_bar_baz.Mumble" and returns the type class
        Mumble.

    Returns:
        The Python type class of the object.
    """
    module_path = i.split(".")
    fidl_import_path = f"{module_path[0]}.{module_path[1]}"
    mod = sys.modules[fidl_import_path]
    obj = mod
    for attr in module_path[2:]:
        obj = getattr(obj, attr)
    return obj


def make_default_obj_from_ident(ident):
    """Takes a FIDL identifier, e.g. foo.bar/Baz, returns the default object (all fields None).

    Args:
        ident: The FIDL identifier.

    Returns:
        The default object construction (all fields None).
    """
    # If there is not identifier then this is for a two way method that returns ().
    if not ident:
        return None
    split = ident.split("/")
    library = "fidl." + split[0].replace(".", "_")
    ty = split[1]
    mod = sys.modules[library]
    return make_default_obj(getattr(mod, ty))


def unwrap_type(ty):
    """Takes a type `ty`, then removes the meta-typing surrounding it.

    Args:
      ty: a Python type.

    Returns:
        The Python type after removing indirection.

    This is because when a user imports fidl.[foo_library], they may import a recursive type, which
    cannot be defined at runtime. This will then return an actual type (since everything will be
    resolvable at this point).
    """
    while True:
        try:
            ty = typing.get_args(ty)[0]
        except IndexError:
            if ty.__class__ is typing.ForwardRef:
                return ty.__forward_arg__
            return ty


def construct_from_name_and_type(constructed_obj, sub_parsed_obj, name, ty):
    unwrapped_ty = unwrap_type(ty)
    if str(unwrapped_ty).startswith("fidl."):
        obj = get_type_from_import(str(unwrapped_ty))
        is_union = False
        try:
            is_union = obj.__fidl_kind__ == "union"
        except AttributeError:
            pass

        def handle_union(parsed_obj):
            if not is_union:
                sub_obj = make_default_obj(obj)
                construct_result(sub_obj, parsed_obj)
            else:
                sub_obj = construct_from_union(obj, parsed_obj)
            return sub_obj

        if isinstance(sub_parsed_obj, dict):
            setattr(constructed_obj, name, handle_union(sub_parsed_obj))
        elif isinstance(sub_parsed_obj, list):
            results = []
            for item in sub_parsed_obj:
                results.append(handle_union(item))
            setattr(constructed_obj, name, results)
        else:
            setattr(constructed_obj, name, sub_parsed_obj)
    else:
        setattr(constructed_obj, name, sub_parsed_obj)


def construct_from_union(obj_type, parsed_obj):
    assert obj_type.__fidl_kind__ == "union"
    assert len(parsed_obj.keys()) == 1
    key = next(iter(parsed_obj.keys()))
    sub_parsed_obj = parsed_obj[key]
    obj_type = getattr(obj_type, key)
    union_type = unwrap_type(obj_type)
    if str(union_type).startswith("fidl."):
        obj = get_type_from_import(str(union_type))
        sub_obj = make_default_obj(obj)
    else:
        sub_obj = make_default_obj(union_type)
    construct_result(sub_obj, sub_parsed_obj)
    return sub_obj


def construct_result(constructed_obj, parsed_obj):
    try:
        elements = type(constructed_obj).__annotations__
    except AttributeError as exc:
        if constructed_obj.__fidl_kind__ == "union":
            construct_from_union(constructed_obj, parsed_obj)
            return
        else:
            raise TypeError(
                f"Unexpected FIDL kind: {constructed_obj.__fidl_kind__}"
            ) from exc
    for name, ty in elements.items():
        if parsed_obj.get(name) is None:
            setattr(constructed_obj, name, None)
            continue
        sub_parsed_obj = parsed_obj[name]
        construct_from_name_and_type(constructed_obj, sub_parsed_obj, name, ty)


async def read_and_decode(chan: fc.FidlChannel):
    global LOOP_STARTED
    loop = asyncio.get_running_loop()
    if not LOOP_STARTED:
        # TODO(fxbug.dev/128194): This assumption (that there is only ever going to be one
        # executor), will cause failures in vanilla rust interfaces that call into Fuchsia
        # controller multiple times via asyncio.run(). This should keep a mapping to publish to more
        # than one executor.
        notification_fd = fc.open_handle_notifier()
        loop.add_reader(
            notification_fd,
            enqueue_ready_zx_handle_from_fd,
            notification_fd,
            HANDLE_READY_QUEUES,
        )
        LOOP_STARTED = True
    if chan.as_int() in HANDLE_READY_QUEUES:
        raise RuntimeError(
            "Only one instance of this handle should be the queue at a time")
    queue: asyncio.Queue[int] = asyncio.Queue(1)
    HANDLE_READY_QUEUES[chan.as_int()] = queue

    def do_read():
        (b, chans) = chan.read()
        res = decode_fidl_response(bytes=b, handles=chans)
        return res

    try:
        result = do_read()
        # In the event that the channel is already ready, remove oneself from the queue.
        HANDLE_READY_QUEUES.pop(chan.as_int())
        return result
    except fc.ZxStatus as e:
        if e.args[0] != fc.ZxStatus.ZX_ERR_SHOULD_WAIT:
            raise e
        loop = asyncio.get_running_loop()
        chan_no = await queue.get()
        assert chan.as_int() == chan_no
    finally:
        # Always want to clear out the ready queue, even if there is an exception (as PEER_CLOSED,
        # for example, will always notify the handle).
        HANDLE_READY_QUEUES.pop(chan.as_int())
    return do_read()


async def send_two_way_fidl_request(
        chan, ordinal, library, msg_obj, response_ident):
    """Sends a two-way asynchronous FIDL request.

    Args:
        chan: The FIDL channel upon which the request is being sent.
        ordinal: The method ordinal (for encoding).
        library: The FIDL library from which this method ordinal exists.
        msg_obj: The object being sent.
        response_ident: The full FIDL identifier of the response object, e.g. foo.bar/Baz

    Returns:
        The object from the two-way function, as constructed from the response_ident type.
    """
    send_one_way_fidl_request(chan, ordinal, library, msg_obj)
    res = await read_and_decode(chan)
    result_obj = make_default_obj_from_ident(response_ident)
    construct_result(result_obj, res)
    return result_obj


def send_one_way_fidl_request(chan, ordinal, library, msg_obj):
    """Sends a synchronous one-way FIDL request.

    Args:
        chan: The FIDL channel upon which the request is being sent.
        ordinal: The method ordinal (for encoding).
        library: The FIDL library from which this method ordinal exists.
        msg_obj: The object being sent.
    """
    global TXID
    TXID += 1
    type_name = None
    if msg_obj is not None and type_name is None:
        type_name = msg_obj.__fidl_type__
    fidl_message = encode_fidl_message(
        ordinal=ordinal,
        object=msg_obj,
        library=library,
        txid=TXID,
        type_name=type_name)
    chan.write(fidl_message)
