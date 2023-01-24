// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_UI_SCENIC_CPP_TESTING_FAKE_FLATLAND_TYPES_H_
#define LIB_UI_SCENIC_CPP_TESTING_FAKE_FLATLAND_TYPES_H_

#include <fuchsia/math/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl_test_base.h>
#include <fuchsia/ui/views/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/interface_handle.h>
#include <lib/fidl/cpp/interface_ptr.h>
#include <lib/fidl/cpp/interface_request.h>
#include <zircon/types.h>

#include <algorithm>
#include <cfloat>
#include <cstdint>
#include <optional>
#include <unordered_map>
#include <variant>
#include <vector>

inline bool operator==(const fuchsia::math::SizeU& a, const fuchsia::math::SizeU& b) {
  return fidl::Equals(a, b);
}

inline bool operator==(const fuchsia::math::Inset& a, const fuchsia::math::Inset& b) {
  return fidl::Equals(a, b);
}

inline bool operator==(const fuchsia::math::Vec& a, const fuchsia::math::Vec& b) {
  return fidl::Equals(a, b);
}

inline bool operator==(const fuchsia::math::VecF& a, const fuchsia::math::VecF& b) {
  return fidl::Equals(a, b);
}

inline bool operator==(const fuchsia::math::Rect& a, const fuchsia::math::Rect& b) {
  return fidl::Equals(a, b);
}

inline bool operator==(const fuchsia::math::RectF& a, const fuchsia::math::RectF& b) {
  return fidl::Equals(a, b);
}

inline bool operator==(const fuchsia::ui::composition::ContentId& a,
                       const fuchsia::ui::composition::ContentId& b) {
  return fidl::Equals(a, b);
}

inline bool operator==(const fuchsia::ui::composition::TransformId& a,
                       const fuchsia::ui::composition::TransformId& b) {
  return fidl::Equals(a, b);
}

inline bool operator==(const fuchsia::ui::composition::ViewportProperties& a,
                       const fuchsia::ui::composition::ViewportProperties& b) {
  return fidl::Equals(a, b);
}

inline bool operator==(const fuchsia::ui::composition::ImageProperties& a,
                       const fuchsia::ui::composition::ImageProperties& b) {
  return fidl::Equals(a, b);
}

inline bool operator==(const fuchsia::ui::composition::HitRegion& a,
                       const fuchsia::ui::composition::HitRegion& b) {
  return fidl::Equals(a, b);
}

inline bool operator!=(const fuchsia::ui::composition::HitRegion& a,
                       const fuchsia::ui::composition::HitRegion& b) {
  return fidl::Equals(a, b);
}

inline bool operator==(const std::vector<fuchsia::ui::composition::HitRegion>& a,
                       const std::vector<fuchsia::ui::composition::HitRegion>& b) {
  if (a.size() != b.size())
    return false;

  for (size_t i = 0; i < a.size(); ++i) {
    if (!fidl::Equals(a, b)) {
      return false;
    }
  }

  return true;
}

inline bool operator==(const std::optional<fuchsia::math::Rect>& a,
                       const std::optional<fuchsia::math::Rect>& b) {
  if (a.has_value() != b.has_value()) {
    return false;
  }
  if (!a.has_value()) {
    return true;
  }
  return fidl::Equals(a.value(), b.value());
}

namespace scenic {

constexpr static fuchsia::ui::composition::TransformId kInvalidTransformId{0};
constexpr static fuchsia::ui::composition::ContentId kInvalidContentId{0};
constexpr static fuchsia::ui::composition::HitRegion kInfiniteHitRegion = {
    .region = {-FLT_MAX, -FLT_MAX, FLT_MAX, FLT_MAX}};

struct FakeView {
  bool operator==(const FakeView& other) const;

  zx_koid_t view_token{};
  zx_koid_t view_ref{};
  zx_koid_t view_ref_control{};
  zx_koid_t view_ref_focused{};
  zx_koid_t focuser{};
  zx_koid_t touch_source{};
  zx_koid_t mouse_source{};
  zx_koid_t parent_viewport_watcher{};
};

struct FakeViewport {
  bool operator==(const FakeViewport& other) const;

  constexpr static fuchsia::math::SizeU kDefaultViewportLogicalSize{};
  constexpr static fuchsia::math::Inset kDefaultViewportInset{};

  fuchsia::ui::composition::ContentId id{kInvalidContentId};

  fuchsia::ui::composition::ViewportProperties viewport_properties{};
  zx_koid_t viewport_token{};
  zx_koid_t child_view_watcher{};
};

struct FakeImage {
  bool operator==(const FakeImage& other) const;

  constexpr static fuchsia::math::SizeU kDefaultImageSize{};
  constexpr static fuchsia::math::RectF kDefaultSampleRegion{};
  constexpr static fuchsia::math::SizeU kDefaultDestinationSize{};
  constexpr static float kDefaultOpacity{1.f};
  constexpr static fuchsia::ui::composition::BlendMode kDefaultBlendMode{
      fuchsia::ui::composition::BlendMode::SRC_OVER};
  constexpr static fuchsia::ui::composition::ImageFlip kDefaultFlip{
      fuchsia::ui::composition::ImageFlip::NONE};

  fuchsia::ui::composition::ContentId id{kInvalidContentId};

  fuchsia::ui::composition::ImageProperties image_properties{};
  fuchsia::math::RectF sample_region{kDefaultSampleRegion};
  fuchsia::math::SizeU destination_size{kDefaultDestinationSize};
  float opacity{kDefaultOpacity};
  fuchsia::ui::composition::BlendMode blend_mode{kDefaultBlendMode};
  fuchsia::ui::composition::ImageFlip flip{kDefaultFlip};

  zx_koid_t collection_id{};
  zx_koid_t import_token{};
  uint32_t vmo_index{0};
};

using FakeContent = std::variant<FakeViewport, FakeImage>;
using FakeContentPtr = std::shared_ptr<FakeContent>;

struct FakeTransform {
  bool operator==(const FakeTransform& other) const;

  constexpr static fuchsia::math::Vec kDefaultTranslation{.x = 0, .y = 0};
  constexpr static fuchsia::math::VecF kDefaultScale{.x = 1.0f, .y = 1.0f};
  constexpr static fuchsia::ui::composition::Orientation kDefaultOrientation{
      fuchsia::ui::composition::Orientation::CCW_0_DEGREES};
  constexpr static float kDefaultOpacity = 1.0f;

  fuchsia::ui::composition::TransformId id{kInvalidTransformId};

  fuchsia::math::Vec translation{kDefaultTranslation};
  fuchsia::math::VecF scale{kDefaultScale};
  fuchsia::ui::composition::Orientation orientation{kDefaultOrientation};

  std::optional<fuchsia::math::Rect> clip_bounds = std::nullopt;

  float opacity = kDefaultOpacity;

  std::vector<std::shared_ptr<FakeTransform>> children;
  std::shared_ptr<FakeContent> content;
  std::vector<fuchsia::ui::composition::HitRegion> hit_regions;
};

using FakeTransformPtr = std::shared_ptr<FakeTransform>;

struct FakeGraph {
  using ContentIdKey = decltype(fuchsia::ui::composition::ContentId::value);
  using TransformIdKey = decltype(fuchsia::ui::composition::TransformId::value);

  bool operator==(const FakeGraph& other) const;

  void Clear();
  FakeGraph Clone() const;

  std::unordered_map<ContentIdKey, std::shared_ptr<FakeContent>> content_map;
  std::unordered_map<TransformIdKey, std::shared_ptr<FakeTransform>> transform_map;
  std::shared_ptr<FakeTransform> root_transform;
  std::optional<FakeView> view;
};

template <typename ZX>
std::pair<zx_koid_t, zx_koid_t> GetKoids(const ZX& kobj) {
  zx_info_handle_basic_t info;
  zx_status_t status = kobj.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info),
                                     /*actual_records*/ nullptr, /*avail_records*/ nullptr);
  return status == ZX_OK ? std::make_pair(info.koid, info.related_koid)
                         : std::make_pair(zx_koid_t{}, zx_koid_t{});
}

template <typename F>
std::pair<zx_koid_t, zx_koid_t> GetKoids(const fidl::InterfaceHandle<F>& interface_handle) {
  return GetKoids(interface_handle.channel());
}

template <typename F>
std::pair<zx_koid_t, zx_koid_t> GetKoids(const fidl::InterfaceRequest<F>& interface_request) {
  return GetKoids(interface_request.channel());
}

template <typename F>
std::pair<zx_koid_t, zx_koid_t> GetKoids(const fidl::InterfacePtr<F>& interface_ptr) {
  return GetKoids(interface_ptr.channel());
}

template <typename F>
std::pair<zx_koid_t, zx_koid_t> GetKoids(const fidl::Binding<F>& interface_binding) {
  return GetKoids(interface_binding.channel());
}

inline std::pair<zx_koid_t, zx_koid_t> GetKoids(
    const fuchsia::ui::views::ViewCreationToken& view_token) {
  return GetKoids(view_token.value);
}

inline std::pair<zx_koid_t, zx_koid_t> GetKoids(
    const fuchsia::ui::views::ViewportCreationToken& viewport_token) {
  return GetKoids(viewport_token.value);
}

inline std::pair<zx_koid_t, zx_koid_t> GetKoids(const fuchsia::ui::views::ViewRef& view_ref) {
  return GetKoids(view_ref.reference);
}

inline std::pair<zx_koid_t, zx_koid_t> GetKoids(
    const fuchsia::ui::views::ViewRefControl& view_ref_control) {
  return GetKoids(view_ref_control.reference);
}

inline std::pair<zx_koid_t, zx_koid_t> GetKoids(
    const fuchsia::ui::composition::BufferCollectionExportToken& buffer_collection_token) {
  return GetKoids(buffer_collection_token.value);
}

inline std::pair<zx_koid_t, zx_koid_t> GetKoids(
    const fuchsia::ui::composition::BufferCollectionImportToken& buffer_collection_token) {
  return GetKoids(buffer_collection_token.value);
}

}  // namespace scenic

#endif  // LIB_UI_SCENIC_CPP_TESTING_FAKE_FLATLAND_TYPES_H_
