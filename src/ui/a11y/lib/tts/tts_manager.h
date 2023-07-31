// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_A11Y_LIB_TTS_TTS_MANAGER_H_
#define SRC_UI_A11Y_LIB_TTS_TTS_MANAGER_H_

#include <fuchsia/accessibility/tts/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/fit/function.h>
#include <lib/sys/cpp/component_context.h>

namespace a11y {

// A class to intermediate interaction between speakers and Tts Engines.
//
// The Tts manager implements |fuchsia.accessibility.tts.TtsManager| and
// |fuchsia.accessibility.tts.EngineRegistry| interfaces. It it registers a
// speaker (assistive technology wanting to produce speech output), as well as a
// Tts engine which is capable of producing the speech output.
// The speaker, after registration, calls methods defined by |fuchsia.accessibility.tts.Engine|,
// which are then forwarded to the registered Tts Engine.
class TtsManager : public fuchsia::accessibility::tts::TtsManager,
                   public fuchsia::accessibility::tts::EngineRegistry,
                   public fuchsia::accessibility::tts::Engine {
 public:
  using TTSEngineReadyCallback = fit::function<void()>;

  // On initialization, this class exposes the services defined in
  // |fuchsia.accessibility.tts.(TtsManager|EngineRegistry|Engine)|
  explicit TtsManager(sys::ComponentContext* startup_context);
  ~TtsManager() override;

  // |fuchsia.accessibility.tts.TtsManager|
  void OpenEngine(fidl::InterfaceRequest<fuchsia::accessibility::tts::Engine> engine_request,
                  OpenEngineCallback callback) override;

  // Unbinds |engine_binding_| if it's bound. Once this call returns, it's safe to call
  // OpenEngine() to open a new engine.
  void CloseEngine();

  // |fuchsia.accessibility.tts.EngineRegistry|
  void RegisterEngine(fidl::InterfaceHandle<fuchsia::accessibility::tts::Engine> engine,
                      RegisterEngineCallback callback) override;

  // Registers a callback that will be invoked once the TTS engine is ready to receive speak
  // requests.
  virtual void RegisterTTSEngineReadyCallback(TTSEngineReadyCallback callback);

  // Unregisters the currently registered callback (if any).
  virtual void UnregisterTTSEngineReadyCallback();

 private:
  // |fuchsia.accessibility.tts.Engine|
  void Enqueue(fuchsia::accessibility::tts::Utterance utterance, EnqueueCallback callback) override;

  // |fuchsia.accessibility.tts.Engine|
  void Speak(SpeakCallback callback) override;

  // |fuchsia.accessibility.tts.Engine|
  void Cancel(CancelCallback callback) override;

  // Executes TTS engine ready callback(s) if both engine and speaker are
  // connected.
  void CheckIfTtsEngineIsReadyAndRunCallback();

  // Bindings to services implemented by this class.
  fidl::BindingSet<fuchsia::accessibility::tts::TtsManager> manager_bindings_;
  fidl::BindingSet<fuchsia::accessibility::tts::EngineRegistry> registry_bindings_;
  fidl::Binding<fuchsia::accessibility::tts::Engine> engine_binding_;

  // Registered engine with this Tts manager. For now, only one engine is
  // allowed to be registered at a time.
  fuchsia::accessibility::tts::EnginePtr engine_;

  // Callback that will be invoked once the TTS engine is ready to receive speak requests.
  TTSEngineReadyCallback tts_engine_ready_callback_;
};

}  // namespace a11y

#endif  // SRC_UI_A11Y_LIB_TTS_TTS_MANAGER_H_
