// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <filesystem>
#include <type_traits>
#include <vector>

#include "error.h"
#include "fuchsia_controller.h"
#include "macros.h"
#include "mod.h"
#include "src/developer/ffx/lib/fuchsia-controller/cpp/python/py_header.h"
#include "src/developer/ffx/lib/fuchsia-controller/cpp/raii/py_wrapper.h"

extern struct PyModuleDef fuchsia_controller_internal;

namespace {

constexpr PyMethodDef SENTINEL = {nullptr, nullptr, 0, nullptr};

/// Enum of python-mapped C++ types
enum PythonTypeID : uint8_t {
  kHandleTypeID = 0,
  kChannelTypeID = 1,
  kContextTypeID = 2,
  kSocketTypeID = 3,
  kIsolateDirTypeID = 4,
  kEventTypeID = 5,
};

const std::vector<const char *> TypeStrings = {"Handle", "Channel",    "Context",
                                               "Socket", "IsolateDir", "Event"};

void SetDowncastError(PyObject *obj, const char *expected) {
  py::Object repr(PyObject_Repr(obj));
  PyErr_Format(PyExc_TypeError, "Failed casting \"%s\", expected %s",
               PyUnicode_AsUTF8AndSize(repr.get(), nullptr), expected);
}

void SetUnknownIdError(PythonTypeID id, const char *expected) {
  PyErr_Format(PyExc_TypeError, "Unable to cast from type %s to \"%s\"", TypeStrings[id], expected);
}

std::pair<std::unique_ptr<ffx_config_t[]>, Py_ssize_t> build_config(PyObject *config,
                                                                    const char *target) {
  static const char TYPE_ERROR[] = "`config` must be a dictionary of string key/value pairs";
  if (!PyDict_Check(config)) {
    PyErr_SetString(PyExc_TypeError, TYPE_ERROR);
    return std::make_pair(nullptr, 0);
  }
  PyObject *maybe_target = PyDict_GetItem(config, PyUnicode_FromString("target.default"));
  if (target && maybe_target) {
    PyErr_Format(
        PyExc_RuntimeError,
        "Context `target` parameter set to '%s', but "
        "config also contains 'target.default' value set to '%s'. You must only specify one",
        target, PyUnicode_AsUTF8AndSize(maybe_target, nullptr));
    return std::make_pair(nullptr, 0);
  }
  Py_ssize_t config_len = PyDict_Size(config);
  if (config_len < 0) {
    return std::make_pair(nullptr, 0);
  }
  std::unique_ptr<ffx_config_t[]> ffx_config;
  if (target) {
    config_len++;
  }
  ffx_config = std::make_unique<ffx_config_t[]>(config_len);

  PyObject *py_key = nullptr;
  PyObject *py_value = nullptr;
  Py_ssize_t pos = 0;
  // `pos` is not used for iterating in ffx_config because it is an internal
  // iterator for a sparse map, so does not always increment by one.
  for (Py_ssize_t i = 0; PyDict_Next(config, &pos, &py_key, &py_value); ++i) {
    if (!PyUnicode_Check(py_key)) {
      PyErr_SetString(PyExc_TypeError, TYPE_ERROR);
      return std::make_pair(nullptr, 0);
    }
    const char *key = PyUnicode_AsUTF8AndSize(py_key, nullptr);
    if (key == nullptr) {
      return std::make_pair(nullptr, 0);
    }
    if (!PyUnicode_Check(py_value)) {
      PyErr_SetString(PyExc_TypeError, TYPE_ERROR);
      return std::make_pair(nullptr, 0);
    }
    const char *value = PyUnicode_AsUTF8AndSize(py_value, nullptr);
    if (value == nullptr) {
      return std::make_pair(nullptr, 0);
    }
    ffx_config[i] = {
        .key = key,
        .value = value,
    };
  }
  if (target) {
    ffx_config[config_len - 1] = {
        .key = "target.default",
        .value = target,
    };
  }
  return std::make_pair(std::move(ffx_config), config_len);
}

/// Base class for Python objects to inherit from.
class PythonObject {
 public:
  explicit PythonObject(PythonTypeID type_id) : type_id_(type_id) {}
  template <typename T>

  /// Dynamically casts into a T if this is an instance of T.
  /// Returns nullptr on failure.
  T *as() {
    if (type_id_ != T::type_id) {
      return nullptr;
    }
    return static_cast<T *>(this);
  }

  /// Converts this PythonObject to a PyObject.
  /// The returned PyObject then assumes ownership of this
  /// PythonObject and will automatically free this object
  /// when the corresponding PyObject is garbage collected
  /// or its reference count hits zero.
  PyObject *IntoPyObject();

  /// Gets the type of this object.
  PythonTypeID GetTypeID() { return type_id_; }

  virtual ~PythonObject() = default;

 private:
  PythonTypeID type_id_;
  bool converted_ = false;
};

/// Generic untyped zx_handle_t.
class PythonHandle : public PythonObject {
 public:
  explicit PythonHandle(zx_handle_t handle) : PythonObject(kHandleTypeID), handle_(handle) {}
  static constexpr PythonTypeID type_id = kHandleTypeID;

  zx_handle_t handle() const { return handle_; }

  zx_handle_t take() {
    auto ret = handle_;
    handle_ = 0;
    return ret;
  }

  ~PythonHandle() override {
    if (handle_) {
      ffx_close_handle(handle_);
    }
  }

 private:
  zx_handle_t handle_;
};

class PythonContext : public PythonObject {
 public:
  explicit PythonContext(ffx_env_context_t *context)
      : PythonObject(kContextTypeID), context_(context) {}
  static constexpr PythonTypeID type_id = kContextTypeID;

  ffx_env_context_t *context() { return context_; }

  ~PythonContext() { destroy_ffx_env_context(context_); }

 private:
  ffx_env_context_t *context_;
};

class PythonChannel : public PythonObject {
 public:
  explicit PythonChannel(zx_handle_t channel) : PythonObject(kChannelTypeID), channel_(channel) {}
  static constexpr PythonTypeID type_id = kChannelTypeID;

  zx_handle_t handle() const { return channel_; }

  uint64_t take() {
    auto ret = channel_;
    channel_ = 0;
    return ret;
  }

  ~PythonChannel() {
    if (channel_) {
      ffx_close_handle(channel_);
      channel_ = 0;
    }
  }

 private:
  zx_handle_t channel_;
};

class PythonSocket : public PythonObject {
 public:
  explicit PythonSocket(zx_handle_t handle) : PythonObject(kSocketTypeID), handle_(handle) {}

  static constexpr PythonTypeID type_id = kSocketTypeID;

  zx_handle_t take() {
    auto handle = handle_;
    handle_ = 0;
    return handle;
  }

  zx_handle_t handle() const { return handle_; }

  ~PythonSocket() {
    if (handle() != 0) {
      ffx_close_handle(handle());
      handle_ = 0;
    }
  }

 private:
  zx_handle_t handle_;
};

class PythonEvent : public PythonObject {
 public:
  explicit PythonEvent(zx_handle_t handle) : PythonObject(kEventTypeID), handle_(handle) {}

  static constexpr PythonTypeID type_id = kEventTypeID;
  zx_handle_t take() {
    auto handle = handle_;
    handle_ = 0;
    return handle;
  }

  zx_handle_t handle() const { return handle_; }

  ~PythonEvent() override {
    if (handle() != 0) {
      ffx_close_handle(handle());
      handle_ = 0;
    }
  }

 private:
  zx_handle_t handle_;
};

class IsolateDir : public PythonObject {
 public:
  explicit IsolateDir(std::filesystem::path path)
      : PythonObject(kIsolateDirTypeID), directory_(std::move(path)) {}

  /// Attempts to create an IsolateDir.
  /// Sets the Python error string and returns nullptr on failure.
  static IsolateDir *Create(const char *dir_cstr) {
    std::filesystem::path directory;
    const char *error_format_str = "Error when creating isolate directory %s: %s";
    if (dir_cstr == nullptr) {
      std::filesystem::path tmp_dir_path = std::filesystem::temp_directory_path() / "fctemp.XXXXXX";

      // Guarantee the temporary directory is created.
      // mkdtemp modifies its parameter in-place, so use it to create a
      // filesystem::path before it goes out of scope
      std::string tmp_dir_str = tmp_dir_path.string();
      if (mkdtemp(tmp_dir_str.data()) == nullptr) {
        const char *error_str = strerror(errno);
        PyErr_Format(PyExc_IOError, error_format_str, tmp_dir_str.c_str(), error_str);
        return nullptr;
      }
      directory = std::filesystem::path(tmp_dir_str);
    } else {
      std::filesystem::path tmp_path = std::filesystem::path(dir_cstr);

      // Guarantee the directory is created.
      std::error_code err;
      if (!std::filesystem::create_directory(tmp_path, err)) {
        // If |err| is falsey this indicates success, although `create_directory`
        // might return false.  This can occur when the directory already exists,
        // so creating it succeeds even though there was an "error".
        //
        // Don't raise a python error in this case.
        if (err) {
          PyErr_Format(PyExc_IOError, error_format_str, tmp_path.c_str(), err.message().c_str());
          return nullptr;
        }
      }
      directory = std::move(tmp_path);
    }
    return new IsolateDir(std::move(directory));
  }

  static constexpr PythonTypeID type_id = kIsolateDirTypeID;

  std::string directory() const { return directory_.string(); }

  ~IsolateDir() { std::filesystem::remove_all(directory_); }

 private:
  std::filesystem::path directory_;
};

extern PyTypeObject *InternalHandleType;

IGNORE_EXTRA_SC
using InternalHandle = struct {
  PyObject_HEAD;
  PythonObject *handle;
};

// Python C ABI requires that this is POD.
static_assert(std::is_pod_v<InternalHandle>);

PyObject *PythonObject::IntoPyObject() {
  if (converted_) {
    fprintf(stderr, "IntoPyObject can only be called once.");
    abort();
  }
  converted_ = true;
  auto handle = PyObject_New(InternalHandle, InternalHandleType);
  handle->handle = this;
  return reinterpret_cast<PyObject *>(handle);
}

/// Constructs a T then converts it into a PyObject, and returns
/// the corresponding PyObject.
template <typename T, typename... Constructor>
PyObject *MakePyObject(Constructor... constructor) {
  return (new T(constructor...))->IntoPyObject();
}

/// Takes the value out of a PythonHandle and converts it to
/// the specified type. The caller is responsible for ensuring
/// this conversion is safe.
/// For the conversion to be safe, the handle must be a PythonHandle,
/// and C++ must not reference the PythonHandle outside of this function.
/// The returned object is owned by the Python runtime, and the old
/// Handle is free'd after this call completes.
template <typename T>
T *HandleCast(InternalHandle *handle) {
  auto newobj = new T(handle->handle->as<PythonHandle>()->take());
  delete handle->handle;
  handle->handle = newobj;
  return newobj;
}

/// Specializations for Channels and Sockets
/// which Python expects handles to be implicitly convertible into.
PythonChannel *DowncastChannel(PyObject *object) {
  if (object->ob_type != InternalHandleType) {
    SetDowncastError(object, "channel");
    return nullptr;
  }
  auto internal_handle = reinterpret_cast<InternalHandle *>(object);
  auto maybe_object = internal_handle->handle->as<PythonChannel>();
  if (maybe_object) {
    return maybe_object;
  }
  auto id = internal_handle->handle->GetTypeID();
  if (id == kHandleTypeID) {
    return HandleCast<PythonChannel>(internal_handle);
  }
  SetUnknownIdError(id, "channel");
  return nullptr;
}

PythonSocket *DowncastSocket(PyObject *object) {
  if (object->ob_type != InternalHandleType) {
    SetDowncastError(object, "socket");
    return nullptr;
  }
  auto internal_handle = reinterpret_cast<InternalHandle *>(object);
  auto maybe_object = internal_handle->handle->as<PythonSocket>();
  if (maybe_object) {
    return maybe_object;
  }
  auto id = internal_handle->handle->GetTypeID();
  if (id == kHandleTypeID) {
    return HandleCast<PythonSocket>(internal_handle);
  }
  SetUnknownIdError(id, "socket");
  return nullptr;
}

PythonEvent *DowncastEvent(PyObject *object) {
  if (object->ob_type != InternalHandleType) {
    SetDowncastError(object, "event");
    return nullptr;
  }
  auto internal_handle = reinterpret_cast<InternalHandle *>(object);
  auto maybe_object = internal_handle->handle->as<PythonEvent>();
  if (maybe_object) {
    return maybe_object;
  }
  auto id = internal_handle->handle->GetTypeID();
  if (id == kEventTypeID) {
    return HandleCast<PythonEvent>(internal_handle);
  }
  SetUnknownIdError(id, "event");
  return nullptr;
}

/// Dynamically downcasts a PyObject to a C++ type.
/// Python retains ownership of the object.
template <typename T>
T *DowncastPyObject(PyObject *object) {
  if (object->ob_type != InternalHandleType) {
    // Object is not a valid handle.
    return nullptr;
  }
  auto internal_handle = reinterpret_cast<InternalHandle *>(object);
  return internal_handle->handle->as<T>();
}

PyObject *handle_from_int(PyObject *self, PyObject *args) {
  unsigned int handle = 0;
  if (!PyArg_ParseTuple(args, "I", &handle)) {
    return nullptr;
  }
  return MakePyObject<PythonHandle>(handle);
}

PyObject *handle_as_int(PyObject *self, PyObject *args) {
  PyObject *object;
  if (!PyArg_ParseTuple(args, "O", &object)) {
    return nullptr;
  }
  // Get C++ object from Python object
  auto handle = DowncastPyObject<PythonHandle>(object);
  if (handle == nullptr) {
    return nullptr;
  }
  return PyLong_FromUnsignedLongLong(handle->handle());
}

template <typename T>
PyObject *handle_koid_helper(PyObject *self, PyObject *args) {
  PyObject *object;
  if (!PyArg_ParseTuple(args, "O", &object)) {
    return nullptr;
  }
  // Get C++ object from Python object
  auto handle = DowncastPyObject<T>(object);
  if (handle == nullptr) {
    return nullptr;
  }
  zx_koid_t out;
  zx_status_t status = ffx_handle_get_koid(mod::get_module_state()->ctx, handle->handle(), &out);
  if (status != ZX_OK) {
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    return nullptr;
  }
  return PyLong_FromUnsignedLongLong(out);
}

PyObject *handle_take(PyObject *self, PyObject *args) {
  PyObject *object;
  if (!PyArg_ParseTuple(args, "O", &object)) {
    return nullptr;
  }
  auto handle = DowncastPyObject<PythonHandle>(object);
  if (handle == nullptr) {
    return nullptr;
  }
  return PyLong_FromUnsignedLongLong(handle->take());
}

void object_dealloc(InternalHandle *self) {
  delete self->handle;
  PyObject_Free(self);
}

PyObject *handle_create(PyObject *self, PyObject *args) {
  uint64_t value = 0;
  if (!PyArg_ParseTuple(args, "K", &value)) {
    return nullptr;
  }
  if (value == 0) {
    return nullptr;
  }
  return reinterpret_cast<PyObject *>(PyObject_New(InternalHandle, InternalHandleType));
}

PyObject *context_create(PyObject *self, PyObject *args) {
  PyObject *config = nullptr;
  const char *isolate = nullptr;
  const char *target = nullptr;
  if (!PyArg_ParseTuple(args, "Osz", &config, &isolate, &target)) {
    return nullptr;
  }
  std::unique_ptr<ffx_config_t[]> ffx_config;
  Py_ssize_t config_len = 0;
  // Convert the borrowed reference into an shared reference to avoid double-free.
  Py_INCREF(config);
  if (!config || config == Py_None) {
    Py_XDECREF(config);
    config = PyDict_New();
  }
  auto pair = build_config(config, target);
  if (pair.first == nullptr) {
    return nullptr;
  }
  ffx_config = std::move(pair.first);
  config_len = pair.second;
  ffx_env_context_t *env_context;
  if (create_ffx_env_context(&env_context, mod::get_module_state()->ctx, ffx_config.get(),
                             config_len, isolate) != ZX_OK) {
    mod::dump_python_err();
    Py_XDECREF(config);
    return nullptr;
  }
  Py_XDECREF(config);
  return MakePyObject<PythonContext>(env_context);
}

PyObject *context_connect_daemon_protocol(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  const char *protocol = nullptr;
  if (!PyArg_ParseTuple(args, "Os", &obj, &protocol)) {
    return nullptr;
  }
  auto context = DowncastPyObject<PythonContext>(obj);
  if (!context) {
    return nullptr;
  }
  zx_handle_t handle;
  zx_status_t status = ffx_connect_daemon_protocol(context->context(), protocol, &handle);
  if (status != ZX_OK) {
    mod::dump_python_err();
    return nullptr;
  }
  return MakePyObject<PythonChannel>(handle);
}

PyObject *context_connect_target_proxy(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  if (!PyArg_ParseTuple(args, "O", &obj)) {
    return nullptr;
  }
  auto context = DowncastPyObject<PythonContext>(obj);
  if (!context) {
    return nullptr;
  }
  zx_handle_t handle;
  zx_status_t status = ffx_connect_target_proxy(context->context(), &handle);
  if (status != ZX_OK) {
    mod::dump_python_err();
    return nullptr;
  }
  return MakePyObject<PythonChannel>(handle);
}

PyObject *context_connect_remote_control_proxy(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  if (!PyArg_ParseTuple(args, "O", &obj)) {
    return nullptr;
  }
  auto context = DowncastPyObject<PythonContext>(obj);
  if (!context) {
    return nullptr;
  }
  zx_handle_t handle;
  zx_status_t status = ffx_connect_remote_control_proxy(context->context(), &handle);
  if (status != ZX_OK) {
    mod::dump_python_err();
    return nullptr;
  }
  return MakePyObject<PythonChannel>(handle);
}

PyObject *context_connect_device_proxy(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  const char *moniker = nullptr;
  const char *capability = nullptr;
  if (!PyArg_ParseTuple(args, "Oss", &obj, &moniker, &capability)) {
    return nullptr;
  }
  auto context = DowncastPyObject<PythonContext>(obj);
  if (!context) {
    return nullptr;
  }
  zx_handle_t handle;
  zx_status_t status = ffx_connect_device_proxy(context->context(), moniker, capability, &handle);
  if (status != ZX_OK) {
    mod::dump_python_err();
    return nullptr;
  }
  return MakePyObject<PythonChannel>(handle);
}

PyObject *context_config_get_string(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  Py_ssize_t key_len;
  const char *key = nullptr;
  if (!PyArg_ParseTuple(args, "Oz#", &obj, &key, &key_len)) {
    return nullptr;
  }
  auto context = DowncastPyObject<PythonContext>(obj);
  if (!context) {
    return nullptr;
  }
  uint64_t buf_size = 4096;
  char buf[buf_size];
  if (zx_status_t res = ffx_config_get_string(context->context(), key,
                                              static_cast<uint64_t>(key_len), buf, &buf_size);
      res != ZX_OK) {
    switch (res) {
      case ZX_ERR_BUFFER_TOO_SMALL:
        PyErr_SetString(PyExc_BufferError, "config key larger than 4096 characters");
        break;
      case ZX_ERR_NOT_FOUND:
        Py_RETURN_NONE;
      default:
        mod::dump_python_err();
    }
    return nullptr;
  }
  return PyUnicode_FromStringAndSize(buf, static_cast<Py_ssize_t>(buf_size));
}

PyObject *connect_handle_notifier(PyObject *self, PyObject *Py_UNUSED(arg)) {
  auto descriptor = ffx_connect_handle_notifier(mod::get_module_state()->ctx);
  if (descriptor <= 0) {
    mod::dump_python_err();
    return nullptr;
  }
  return PyLong_FromLong(descriptor);
}

PyObject *context_target_add(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  const char *target = nullptr;
  bool wait = false;
  if (!PyArg_ParseTuple(args, "Osb", &obj, &target, &wait)) {
    return nullptr;
  }
  auto context = DowncastPyObject<PythonContext>(obj);
  if (!context) {
    return nullptr;
  }
  zx_status_t status = ffx_target_add(context->context(), target, wait);
  if (status != ZX_OK) {
    switch (status) {
      case ZX_ERR_INVALID_ARGS:
        PyErr_Format(PyExc_ValueError, "invalid address passed as target: '%s'", target);
        break;
      default:
        mod::dump_python_err();
    }
    return nullptr;
  }
  Py_RETURN_NONE;
}

PyObject *context_target_wait(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  double seconds = 0;
  if (!PyArg_ParseTuple(args, "Od", &obj, &seconds)) {
    return nullptr;
  }
  auto context = DowncastPyObject<PythonContext>(obj);
  if (!context) {
    return nullptr;
  }
  zx_status_t status = ffx_target_wait(context->context(), seconds);
  if (status != ZX_OK) {
    mod::dump_python_err();
    return nullptr;
  }
  Py_RETURN_NONE;
}

PyObject *channel_write(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  PyObject *bytes = nullptr;
  PyObject *handles = nullptr;
  if (!PyArg_ParseTuple(args, "OOO", &obj, &bytes, &handles)) {
    return nullptr;
  }
  auto channel = DowncastChannel(obj);
  if (!channel) {
    return nullptr;
  }
  constexpr size_t max_handle_count = 64;
  zx_handle_disposition_t c_handles[max_handle_count];

  Py_buffer view;
  if (PyObject_GetBuffer(handles, &view, PyBUF_CONTIG_RO) < 0) {
    PyErr_SetString(PyExc_TypeError, "Expected a buffer.");
    return nullptr;
  }
  size_t handles_len = view.len / sizeof(zx_handle_disposition_t);
  if (static_cast<size_t>(view.len) > sizeof(c_handles)) {
    PyErr_SetString(PyExc_TypeError, "Only 64 handles are allowed in a FIDL message.");
    PyBuffer_Release(&view);
    return nullptr;
  }
  // We need the memcpy because Python doesn't guarantee that objects are aligned to 4 bytes.
  // We could have UB on the Rust side if we don't properly align this to 4 bytes during the
  // channel_write_etc call.
  memcpy(c_handles, view.buf, view.len);
  PyBuffer_Release(&view);
  if (PyObject_GetBuffer(bytes, &view, PyBUF_CONTIG_RO) < 0) {
    PyErr_SetString(PyExc_TypeError, "Expected a buffer.");
    return nullptr;
  }
  zx_status_t status =
      ffx_channel_write_etc(mod::get_module_state()->ctx, channel->handle(),
                            static_cast<const char *>(view.buf), view.len, c_handles, handles_len);
  if (status != ZX_OK) {
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    PyBuffer_Release(&view);
    return nullptr;
  }
  PyBuffer_Release(&view);
  Py_RETURN_NONE;
}

PyObject *channel_read(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  if (!PyArg_ParseTuple(args, "O", &obj)) {
    return nullptr;
  }
  auto channel = DowncastChannel(obj);
  if (!channel) {
    return nullptr;
  }
  // This is the max FIDL message size;
  static constexpr uint64_t c_buf_len = 65536;
  static char c_buf[c_buf_len] = {};
  static constexpr uint64_t handles_len = 64;
  static zx_handle_t handles[handles_len] = {};
  static uint64_t actual_bytes_count = 0;
  static uint64_t actual_handles_count = 0;

  auto status = ffx_channel_read(mod::get_module_state()->ctx, channel->handle(), c_buf, c_buf_len,
                                 handles, handles_len, &actual_bytes_count, &actual_handles_count);
  if (status != ZX_OK) {
    // Lint suppressed because we are asserting on the sizeof(long),
    // not int64.
    static_assert(sizeof(long) >= sizeof(status));  // NOLINT
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    return nullptr;
  }
  auto res = py::Object(PyTuple_New(2));
  if (res == nullptr) {
    return nullptr;
  }
  auto buf = py::Object(PyByteArray_FromStringAndSize(const_cast<const char *>(c_buf),
                                                      static_cast<Py_ssize_t>(actual_bytes_count)));
  if (buf == nullptr) {
    return nullptr;
  }
  auto handles_list = py::Object(PyList_New(static_cast<Py_ssize_t>(actual_handles_count)));
  if (handles_list == nullptr) {
    return nullptr;
  }
  PyTuple_SetItem(res.get(), 0, buf.take());
  for (uint64_t i = 0; i < actual_handles_count; ++i) {
    zx_handle_t handle = handles[i];
    auto handle_obj = MakePyObject<PythonHandle>(handle);
    PyList_SetItem(handles_list.get(), static_cast<Py_ssize_t>(i), handle_obj);
  }
  PyTuple_SetItem(res.get(), 1, handles_list.take());
  return res.take();
}

PyObject *socket_write(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  PyObject *data = nullptr;
  if (!PyArg_ParseTuple(args, "OO", &obj, &data)) {
    return nullptr;
  }
  auto socket = DowncastSocket(obj);
  if (!socket) {
    return nullptr;
  }
  Py_buffer view;
  if (PyObject_GetBuffer(data, &view, PyBUF_CONTIG_RO) < 0) {
    PyErr_SetString(PyExc_TypeError, "Expected a buffer.");
    return nullptr;
  }
  auto status = ffx_socket_write(mod::get_module_state()->ctx, socket->handle(),
                                 static_cast<const char *>(view.buf), view.len);
  PyBuffer_Release(&view);
  if (status != ZX_OK) {
    // Lint suppressed because we are asserting on the sizeof(long),
    // not int64.
    static_assert(sizeof(long) >= sizeof(status));  // NOLINT
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    return nullptr;
  }
  Py_RETURN_NONE;
}

PyObject *socket_read(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  if (!PyArg_ParseTuple(args, "O", &obj)) {
    return nullptr;
  }
  auto socket = DowncastSocket(obj);
  if (!socket) {
    return nullptr;
  }
  constexpr static uint64_t c_buf_len = 65536;
  static char c_buf[c_buf_len] = {};
  uint64_t actual_bytes_count = 0;
  auto status = ffx_socket_read(mod::get_module_state()->ctx, socket->handle(), c_buf,
                                sizeof(c_buf), &actual_bytes_count);
  if (status != ZX_OK) {
    // Lint suppressed because we are asserting on the sizeof(long),
    // not int64.
    static_assert(sizeof(long) >= sizeof(status));  // NOLINT
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    return nullptr;
  }
  return PyByteArray_FromStringAndSize(c_buf, static_cast<ssize_t>(actual_bytes_count));
}

PyObject *socket_as_int(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  if (!PyArg_ParseTuple(args, "O", &obj)) {
    return nullptr;
  }
  auto socket = DowncastSocket(obj);
  if (!socket) {
    return nullptr;
  }
  return PyLong_FromUnsignedLongLong(socket->handle());
}

PyObject *socket_take(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  if (!PyArg_ParseTuple(args, "O", &obj)) {
    return nullptr;
  }
  auto socket = DowncastSocket(obj);
  if (!socket) {
    return nullptr;
  }
  auto handle = socket->take();
  return PyLong_FromUnsignedLongLong(handle);
}

template <typename... Args>
PyObject *MakePythonTuple(Args... args) {
  PyObject *tuple = PyTuple_New(sizeof...(Args));
  if (!tuple) {
    return nullptr;
  }
  PyObject *items[] = {args...};
  for (size_t i = 0; i < sizeof...(Args); i++) {
    PyTuple_SetItem(tuple, i, items[i]);
  }
  return tuple;
}

PyObject *socket_create(PyObject *self, PyObject *args) {
  unsigned int options;
  if (!PyArg_ParseTuple(args, "I", &options)) {
    return nullptr;
  }
  zx_handle_t socket1 = ZX_HANDLE_INVALID;
  zx_handle_t socket2 = ZX_HANDLE_INVALID;
  auto status = ffx_socket_create(mod::get_module_state()->ctx, options, &socket1, &socket2);
  if (status != ZX_OK) {
    // Lint suppressed because we are asserting on the sizeof(long),
    // not int64.
    static_assert(sizeof(long) >= sizeof(status));  // NOLINT
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    return nullptr;
  }
  return MakePythonTuple(MakePyObject<PythonSocket>(socket1), MakePyObject<PythonSocket>(socket2));
}

PyObject *channel_as_int(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  if (!PyArg_ParseTuple(args, "O", &obj)) {
    return nullptr;
  }
  auto channel = DowncastChannel(obj);
  if (!channel) {
    return nullptr;
  }
  return PyLong_FromUnsignedLongLong(channel->handle());
}

PyObject *channel_take(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  if (!PyArg_ParseTuple(args, "O", &obj)) {
    return nullptr;
  }
  auto channel = DowncastChannel(obj);
  if (!channel) {
    return nullptr;
  }
  auto handle = channel->take();
  return PyLong_FromUnsignedLongLong(handle);
}

PyObject *channel_create(PyObject *self, PyObject *args) {
  zx_handle_t hdl0;
  zx_handle_t hdl1;
  ffx_channel_create(mod::get_module_state()->ctx, 0, &hdl0, &hdl1);
  py::Object tuple(PyTuple_New(2));
  if (tuple == nullptr) {
    return nullptr;
  }
  PyTuple_SetItem(tuple.get(), 0, MakePyObject<PythonChannel>(hdl0));
  PyTuple_SetItem(tuple.get(), 1, MakePyObject<PythonChannel>(hdl1));
  return tuple.take();
}

PyObject *channel_from_int(PyObject *self, PyObject *args) {
  unsigned int handle = 0;
  if (!PyArg_ParseTuple(args, "I", &handle)) {
    return nullptr;
  }
  return MakePyObject<PythonChannel>(handle);
}

PyObject *event_from_int(PyObject *self, PyObject *args) {
  unsigned int handle = 0;
  if (!PyArg_ParseTuple(args, "I", &handle)) {
    return nullptr;
  }
  return MakePyObject<PythonEvent>(handle);
}

PyObject *event_signal_peer(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  unsigned int clear_mask;
  unsigned int set_mask;
  if (!PyArg_ParseTuple(args, "OII", &obj, &clear_mask, &set_mask)) {
    return nullptr;
  }
  auto event = DowncastPyObject<PythonEvent>(obj);
  if (!event) {
    return nullptr;
  }
  zx_status_t status =
      ffx_object_signal_peer(mod::get_module_state()->ctx, event->handle(), clear_mask, set_mask);
  if (status != ZX_OK) {
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    return nullptr;
  }
  Py_RETURN_NONE;
}

PyObject *event_as_int(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  if (!PyArg_ParseTuple(args, "O", &obj)) {
    return nullptr;
  }
  auto event = DowncastEvent(obj);
  if (!event) {
    return nullptr;
  }
  return PyLong_FromUnsignedLongLong(event->handle());
}

PyObject *event_take(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  if (!PyArg_ParseTuple(args, "O", &obj)) {
    return nullptr;
  }
  auto event = DowncastEvent(obj);
  if (!event) {
    return nullptr;
  }
  auto handle = event->take();
  return PyLong_FromUnsignedLongLong(handle);
}

PyObject *event_create(PyObject *self, PyObject *args) {
  zx_handle_t hdl;
  auto status = ffx_event_create(mod::get_module_state()->ctx, 0, &hdl);
  if (status != ZX_OK) {
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    return nullptr;
  }
  return MakePyObject<PythonEvent>(hdl);
}

PyObject *event_create_pair(PyObject *self, PyObject *args) {
  zx_handle_t hdl0;
  zx_handle_t hdl1;
  auto status = ffx_eventpair_create(mod::get_module_state()->ctx, 0, &hdl0, &hdl1);
  if (status != ZX_OK) {
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    return nullptr;
  }
  py::Object tuple(PyTuple_New(2));
  if (tuple == nullptr) {
    return nullptr;
  }
  PyTuple_SetItem(tuple.get(), 0, MakePyObject<PythonEvent>(hdl0));
  PyTuple_SetItem(tuple.get(), 1, MakePyObject<PythonEvent>(hdl1));
  return tuple.take();
}

PyObject *socket_from_int(PyObject *self, PyObject *args) {
  unsigned int handle = 0;
  if (!PyArg_ParseTuple(args, "I", &handle)) {
    return nullptr;
  }
  return MakePyObject<PythonSocket>(handle);
}

PyObject *isolate_dir_create(PyObject *self, PyObject *args) {
  const char *maybe_cstr = nullptr;
  if (!PyArg_ParseTuple(args, "z", &maybe_cstr)) {
    return nullptr;
  }
  auto maybe_directory = IsolateDir::Create(maybe_cstr);
  // No need to set the error, as IsolateDir::Create does that for us based
  // on the error returned from the C++ standard library.
  if (!maybe_directory) {
    return nullptr;
  }
  return maybe_directory->IntoPyObject();
}

PyObject *isolate_dir_get_path(PyObject *self, PyObject *args) {
  PyObject *obj = nullptr;
  if (!PyArg_ParseTuple(args, "O", &obj)) {
    return nullptr;
  }
  auto directory = DowncastPyObject<IsolateDir>(obj);
  if (!directory) {
    return nullptr;
  }
  return PyUnicode_FromString(directory->directory().c_str());
}

PyType_Slot InternalHandleType_slots[] = {
    {Py_tp_dealloc, reinterpret_cast<void *>(object_dealloc)},
    {Py_tp_doc, reinterpret_cast<void *>(
                    const_cast<char *>("Internal handle used by fuchsia controller for FFI."))},
    {0, nullptr},
};

PyType_Spec InternalHandleType_Spec = {
    .name = "fuchsia_controller_internal.InternalHandle",
    .basicsize = sizeof(InternalHandle),
    .itemsize = 0,
    .slots = InternalHandleType_slots,
};

PyTypeObject *InternalHandleType = nullptr;

PyMethodDef FuchsiaControllerMethods[] = {
    // v2 methods for handle
    {"handle_from_int", reinterpret_cast<PyCFunction>(handle_from_int), METH_VARARGS, nullptr},
    {"handle_as_int", reinterpret_cast<PyCFunction>(handle_as_int), METH_VARARGS, nullptr},
    {"handle_take", reinterpret_cast<PyCFunction>(handle_take), METH_VARARGS, nullptr},
    {"handle_create", reinterpret_cast<PyCFunction>(handle_create), METH_VARARGS, nullptr},
    {"handle_koid", reinterpret_cast<PyCFunction>(handle_koid_helper<PythonHandle>), METH_VARARGS,
     nullptr},

    // v2 methods for Context
    {"context_create", reinterpret_cast<PyCFunction>(context_create), METH_VARARGS, nullptr},
    {"context_connect_daemon_protocol",
     reinterpret_cast<PyCFunction>(context_connect_daemon_protocol), METH_VARARGS, nullptr},
    {"context_connect_target_proxy", reinterpret_cast<PyCFunction>(context_connect_target_proxy),
     METH_VARARGS, nullptr},
    {"context_connect_device_proxy", reinterpret_cast<PyCFunction>(context_connect_device_proxy),
     METH_VARARGS, nullptr},
    {"context_connect_remote_control_proxy",
     reinterpret_cast<PyCFunction>(context_connect_remote_control_proxy), METH_VARARGS, nullptr},
    {"context_target_add", reinterpret_cast<PyCFunction>(context_target_add), METH_VARARGS,
     nullptr},
    {"context_target_wait", reinterpret_cast<PyCFunction>(context_target_wait), METH_VARARGS,
     nullptr},
    {"context_config_get_string", reinterpret_cast<PyCFunction>(context_config_get_string),
     METH_VARARGS, nullptr},

    // v2 methods for channel
    {"channel_read", reinterpret_cast<PyCFunction>(channel_read), METH_VARARGS, nullptr},
    {"channel_write", reinterpret_cast<PyCFunction>(channel_write), METH_VARARGS, nullptr},
    {"channel_as_int", reinterpret_cast<PyCFunction>(channel_as_int), METH_VARARGS, nullptr},
    {"channel_take", reinterpret_cast<PyCFunction>(channel_take), METH_VARARGS, nullptr},
    {"channel_create", reinterpret_cast<PyCFunction>(channel_create), METH_NOARGS, nullptr},
    {"channel_from_int", reinterpret_cast<PyCFunction>(channel_from_int), METH_VARARGS, nullptr},
    {"channel_koid", reinterpret_cast<PyCFunction>(handle_koid_helper<PythonChannel>), METH_VARARGS,
     nullptr},

    // v2 methods for socket
    {"socket_read", reinterpret_cast<PyCFunction>(socket_read), METH_VARARGS, nullptr},
    {"socket_write", reinterpret_cast<PyCFunction>(socket_write), METH_VARARGS, nullptr},
    {"socket_as_int", reinterpret_cast<PyCFunction>(socket_as_int), METH_VARARGS, nullptr},
    {"socket_take", reinterpret_cast<PyCFunction>(socket_take), METH_VARARGS, nullptr},
    {"socket_create", reinterpret_cast<PyCFunction>(socket_create), METH_VARARGS, nullptr},
    {"connect_handle_notifier", reinterpret_cast<PyCFunction>(connect_handle_notifier),
     METH_VARARGS, nullptr},
    {"socket_from_int", reinterpret_cast<PyCFunction>(socket_from_int), METH_VARARGS, nullptr},
    {"socket_koid", reinterpret_cast<PyCFunction>(handle_koid_helper<PythonSocket>), METH_VARARGS,
     nullptr},

    // event methods
    {"event_from_int", reinterpret_cast<PyCFunction>(event_from_int), METH_VARARGS, nullptr},
    {"event_as_int", reinterpret_cast<PyCFunction>(event_as_int), METH_VARARGS, nullptr},
    {"event_take", reinterpret_cast<PyCFunction>(event_take), METH_VARARGS, nullptr},
    {"event_signal_peer", reinterpret_cast<PyCFunction>(event_signal_peer), METH_VARARGS, nullptr},
    {"event_create", reinterpret_cast<PyCFunction>(event_create), METH_NOARGS, nullptr},
    {"event_create_pair", reinterpret_cast<PyCFunction>(event_create_pair), METH_NOARGS, nullptr},
    {"event_koid", reinterpret_cast<PyCFunction>(handle_koid_helper<PythonEvent>), METH_VARARGS,
     nullptr},

    // v2 methods for IsolateDir (create, get name)
    {"isolate_dir_create", reinterpret_cast<PyCFunction>(isolate_dir_create), METH_VARARGS,
     nullptr},
    {"isolate_dir_get_path", reinterpret_cast<PyCFunction>(isolate_dir_get_path), METH_VARARGS,
     nullptr},

    SENTINEL,
};

int FuchsiaControllerModule_clear(PyObject *m) {
  auto state = reinterpret_cast<mod::FuchsiaControllerState *>(PyModule_GetState(m));
  destroy_ffx_lib_context(state->ctx);
  return 0;
}

int InternalHandleTypeInit() {
  return mod::GenericTypeInit(&InternalHandleType, &InternalHandleType_Spec);
}

PyMODINIT_FUNC __attribute__((visibility("default"))) PyInit_fuchsia_controller_internal() {
  if (InternalHandleTypeInit() < 0) {
    return nullptr;
  }
  auto m = py::Object(PyModule_Create(&fuchsia_controller_internal));
  if (m == nullptr) {
    return nullptr;
  }
  auto zx_status_type = error::ZxStatusType_Create();
  if (zx_status_type == nullptr) {
    return nullptr;
  }
  auto state = reinterpret_cast<mod::FuchsiaControllerState *>(PyModule_GetState(m.get()));
  create_ffx_lib_context(&state->ctx, state->ERR_SCRATCH, mod::ERR_SCRATCH_LEN);
  if (PyModule_AddObject(m.get(), "ZxStatus", PyObjCast(zx_status_type)) < 0) {
    Py_DECREF(zx_status_type);
    return nullptr;
  }
  if (PyModule_AddObject(m.get(), "InternalHandle", PyObjCast(InternalHandleType)) < 0) {
    return nullptr;
  }
  return m.take();
}

}  // namespace

DES_MIX struct PyModuleDef fuchsia_controller_internal = {
    PyModuleDef_HEAD_INIT,
    .m_name = "fuchsia_controller_internal",
    .m_doc = nullptr,
    .m_size = sizeof(mod::FuchsiaControllerState),
    .m_methods = FuchsiaControllerMethods,
    .m_clear = FuchsiaControllerModule_clear,
};
