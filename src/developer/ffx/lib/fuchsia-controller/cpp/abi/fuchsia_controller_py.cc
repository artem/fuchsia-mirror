// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#define PY_SSIZE_T_CLEAN

#include <Python.h>

#include "channel.h"
#include "error.h"
#include "fuchsia_controller.h"
#include "isolate_directory.h"
#include "macros.h"
#include "mod.h"
#include "src/developer/ffx/lib/fuchsia-controller/cpp/raii/py_wrapper.h"

extern struct PyModuleDef fuchsia_controller_py;

namespace {

constexpr PyMethodDef SENTINEL = {nullptr, nullptr, 0, nullptr};

IGNORE_EXTRA_SC
using Context = struct {
  PyObject_HEAD;
  ffx_env_context_t *env_context;
  PyObject *isolate_dir;
};

void Context_dealloc(Context *self) {
  destroy_ffx_env_context(self->env_context);
  Py_XDECREF(self->isolate_dir);
  Py_TYPE(self)->tp_free(reinterpret_cast<PyObject *>(self));
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
        target, PyUnicode_AsUTF8(maybe_target));
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
    const char *key = PyUnicode_AsUTF8(py_key);
    if (key == nullptr) {
      return std::make_pair(nullptr, 0);
    }
    if (!PyUnicode_Check(py_value)) {
      PyErr_SetString(PyExc_TypeError, TYPE_ERROR);
      return std::make_pair(nullptr, 0);
    }
    const char *value = PyUnicode_AsUTF8(py_value);
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

int Context_init(Context *self, PyObject *args, PyObject *kwds) {
  static const char *kwlist[] = {"config", "isolate_dir", "target", nullptr};
  PyObject *config = nullptr;
  PyObject *isolate = nullptr;
  const char *target = nullptr;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "|OOz", const_cast<char **>(kwlist), &config,
                                   &isolate, &target)) {
    return -1;
  }
  if (isolate &&
      !PyObject_IsInstance(isolate, reinterpret_cast<PyObject *>(&isolate::IsolateDirType))) {
    PyErr_SetString(PyExc_TypeError, "isolate must be a fuchsia_controller_py.IsolateDir type");
    Py_XDECREF(config);
    Py_XDECREF(isolate);
    return -1;
  }
  // This temp incref/decref stuff is just derived from the tutorial:
  // https://docs.python.org/3/extending/newtypes_tutorial.html
  PyObject *tmp = self->isolate_dir;
  Py_XINCREF(isolate);
  self->isolate_dir = isolate;
  Py_XDECREF(tmp);

  std::unique_ptr<ffx_config_t[]> ffx_config;
  Py_ssize_t config_len = 0;
  if (!config || config == Py_None) {
    Py_XDECREF(config);
    config = PyDict_New();
  }
  auto pair = build_config(config, target);
  if (pair.first == nullptr) {
    Py_XDECREF(config);
    Py_XDECREF(isolate);
    return -1;
  }
  ffx_config = std::move(pair.first);
  config_len = pair.second;
  const char *isolate_dir = nullptr;
  if (isolate) {
    isolate_dir = reinterpret_cast<isolate::IsolateDir *>(isolate)->dir.c_str();
  }
  if (create_ffx_env_context(&self->env_context, mod::get_module_state()->ctx, ffx_config.get(),
                             config_len, isolate_dir) != ZX_OK) {
    mod::dump_python_err();
    Py_XDECREF(config);
    Py_XDECREF(isolate);
    return -1;
  }
  return 0;
}

PyObject *Context_connect_daemon_protocol(Context *self, PyObject *protocol) {
  const char *c_protocol = PyUnicode_AsUTF8(protocol);
  if (c_protocol == nullptr) {
    return nullptr;
  }
  zx_handle_t handle;
  if (ffx_connect_daemon_protocol(self->env_context, c_protocol, &handle) != ZX_OK) {
    mod::dump_python_err();
    return nullptr;
  }
  return PyObject_CallFunction(reinterpret_cast<PyObject *>(&channel::ChannelType), "I", handle);
}

PyObject *Context_connect_target_proxy(Context *self, PyObject *Py_UNUSED(unused)) {
  zx_handle_t handle;
  if (ffx_connect_target_proxy(self->env_context, &handle) != ZX_OK) {
    mod::dump_python_err();
    return nullptr;
  }
  return PyObject_CallFunction(reinterpret_cast<PyObject *>(&channel::ChannelType), "I", handle);
}

PyObject *Context_connect_device_proxy(Context *self, PyObject *args) {
  char *c_moniker;
  char *c_capability_name;
  if (!PyArg_ParseTuple(args, "ss", &c_moniker, &c_capability_name)) {
    return nullptr;
  }
  zx_handle_t handle;
  if (ffx_connect_device_proxy(self->env_context, c_moniker, c_capability_name, &handle) != ZX_OK) {
    mod::dump_python_err();
    return nullptr;
  }
  return PyObject_CallFunction(reinterpret_cast<PyObject *>(&channel::ChannelType), "I", handle);
}

PyMethodDef Context_methods[] = {
    {"connect_daemon_protocol", reinterpret_cast<PyCFunction>(Context_connect_daemon_protocol),
     METH_O, nullptr},
    {"connect_target_proxy", reinterpret_cast<PyCFunction>(Context_connect_target_proxy),
     METH_NOARGS, nullptr},
    {"connect_device_proxy", reinterpret_cast<PyCFunction>(Context_connect_device_proxy),
     METH_VARARGS, nullptr},
    SENTINEL};

DES_MIX PyTypeObject ContextType = {
    PyVarObject_HEAD_INIT(nullptr, 0)

        .tp_name = "fuchsia_controller_py.Context",
    .tp_basicsize = sizeof(Context),
    .tp_itemsize = 0,
    .tp_dealloc = reinterpret_cast<destructor>(Context_dealloc),
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_doc =
        "Fuchsia controller context. This is the necessary object for interacting with a Fuchsia device.",
    .tp_methods = Context_methods,
    .tp_init = reinterpret_cast<initproc>(Context_init),
    .tp_new = PyType_GenericNew,
};

PyObject *connect_handle_notifier(PyObject *self, PyObject *Py_UNUSED(arg)) {
  auto descriptor = ffx_connect_handle_notifier(mod::get_module_state()->ctx);
  if (descriptor <= 0) {
    mod::dump_python_err();
    return nullptr;
  }
  return PyLong_FromLong(descriptor);
}

PyMethodDef FuchsiaControllerMethods[] = {
    {"connect_handle_notifier", connect_handle_notifier, METH_NOARGS,
     "Open the handle notification descriptor. Can only be done once within a module."},
    SENTINEL,
};

int FuchsiaControllerModule_clear(PyObject *m) {
  auto state = reinterpret_cast<mod::FuchsiaControllerState *>(PyModule_GetState(m));
  destroy_ffx_lib_context(state->ctx);
  return 0;
}

PyMODINIT_FUNC __attribute__((visibility("default"))) PyInit_fuchsia_controller_py() {
  if (PyType_Ready(&ContextType) < 0) {
    return nullptr;
  }
  if (PyType_Ready(&handle::HandleType) < 0) {
    return nullptr;
  }
  if (PyType_Ready(&channel::ChannelType) < 0) {
    return nullptr;
  }
  if (PyType_Ready(&isolate::IsolateDirType) < 0) {
    return nullptr;
  }
  auto m = py::Object(PyModule_Create(&fuchsia_controller_py));
  if (m == nullptr) {
    return nullptr;
  }
  auto state = reinterpret_cast<mod::FuchsiaControllerState *>(PyModule_GetState(m.get()));
  create_ffx_lib_context(&state->ctx, state->ERR_SCRATCH, mod::ERR_SCRATCH_LEN);
  Py_INCREF(&ContextType);
  if (PyModule_AddObject(m.get(), "Context", reinterpret_cast<PyObject *>(&ContextType)) < 0) {
    Py_DECREF(&ContextType);
    return nullptr;
  }
  if (PyModule_AddObject(m.get(), "IsolateDir",
                         reinterpret_cast<PyObject *>(&isolate::IsolateDirType)) < 0) {
    Py_DECREF(&isolate::IsolateDirType);
    return nullptr;
  }
  auto zx_status_type = error::ZxStatusType_Create();
  if (PyModule_AddObject(m.get(), "ZxStatus", reinterpret_cast<PyObject *>(zx_status_type)) < 0) {
    Py_DECREF(zx_status_type);
    return nullptr;
  }
  Py_INCREF(&handle::HandleType);
  if (PyModule_AddObject(m.get(), "Handle", reinterpret_cast<PyObject *>(&handle::HandleType)) <
      0) {
    Py_DECREF(&handle::HandleType);
    return nullptr;
  }
  Py_INCREF(&channel::ChannelType);
  if (PyModule_AddObject(m.get(), "Channel", reinterpret_cast<PyObject *>(&channel::ChannelType)) <
      0) {
    Py_DECREF(&channel::ChannelType);
    return nullptr;
  }
  return m.take();
}

}  // namespace

DES_MIX struct PyModuleDef fuchsia_controller_py = {
    PyModuleDef_HEAD_INIT,
    .m_name = "fuchsia_controller_py",
    .m_doc = nullptr,
    .m_size = sizeof(mod::FuchsiaControllerState),
    .m_methods = FuchsiaControllerMethods,
    .m_clear = FuchsiaControllerModule_clear,
};
