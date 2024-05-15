// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_SIGNAL_PROCESSING_UTILS_UNITTEST_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_SIGNAL_PROCESSING_UTILS_UNITTEST_H_

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>

#include <vector>

#include "src/media/audio/services/device_registry/basic_types.h"

namespace media_audio {

namespace fhasp = fuchsia_hardware_audio_signalprocessing;

// Element ids
constexpr ElementId kAgcElementId = 2;
constexpr ElementId kDaiEndpointElementId = 1;
constexpr ElementId kRingBufferEndpointElementId = 0;
constexpr ElementId kDynamicsElementId = 42;
constexpr ElementId kEqualizerElementId = 55;
constexpr ElementId kGainElementId = 555;
constexpr ElementId kVendorSpecificElementId = 10164;
constexpr ElementId kBadElementId = 68;

// Elements
const fhasp::Element kAgcElement{{
    .id = kAgcElementId,
    .type = fhasp::ElementType::kAutomaticGainControl,
    .description = std::string("Test signalprocessing description for an automatic-gain-control ") +
                   std::string("element with expected functionality.  As intentionally defined, ") +
                   std::string("this description is a maximal-length 256-char string. Note that ") +
                   std::string("this string has an upper-case 'x' as the last character.  54321X"),
    .can_stop = false,
}};
const fhasp::Element kDaiEndpointElement{{
    .id = kDaiEndpointElementId,
    .type = fhasp::ElementType::kEndpoint,
    .type_specific = fhasp::TypeSpecificElement::WithEndpoint({{
        .type = fhasp::EndpointType::kDaiInterconnect,
        .plug_detect_capabilities = fhasp::PlugDetectCapabilities::kCanAsyncNotify,
    }}),
    .description = " ",
    .can_stop = true,
    .can_bypass = false,
}};
const fhasp::Element kRingBufferElement{{
    .id = kRingBufferEndpointElementId,  //
    .type = fhasp::ElementType::kRingBuffer,
    // .type_specific is missing
    // .description is missing
    // .can_stop is missing
    // .can_bypass is missing
}};
const fhasp::Element kDynamicsElement{{
    .id = kDynamicsElementId,
    .type = fhasp::ElementType::kDynamics,
    .type_specific = fhasp::TypeSpecificElement::WithDynamics({{
        .bands = {{{{1}}, {{2}}}},
        .supported_controls = fhasp::DynamicsSupportedControls::kKneeWidth |
                              fhasp::DynamicsSupportedControls::kAttack |
                              fhasp::DynamicsSupportedControls::kRelease |
                              fhasp::DynamicsSupportedControls::kOutputGain |
                              fhasp::DynamicsSupportedControls::kInputGain |
                              fhasp::DynamicsSupportedControls::kLookahead |
                              fhasp::DynamicsSupportedControls::kLevelType |
                              fhasp::DynamicsSupportedControls::kLinkedChannels |
                              fhasp::DynamicsSupportedControls::kThresholdType,
    }}),
    .description = "Generic Dynamics element with all capabilities",
    .can_bypass = true,
}};
const fhasp::Element kEqualizerElement{{
    .id = kEqualizerElementId,
    .type = fhasp::ElementType::kEqualizer,
    .type_specific = fhasp::TypeSpecificElement::WithEqualizer({{
        .bands = {{{{1}}, {{2}}, {{3}}, {{4}}, {{5}}, {{6}}}},
        .supported_controls = fhasp::EqualizerSupportedControls::kCanControlFrequency |
                              fhasp::EqualizerSupportedControls::kCanControlQ |
                              fhasp::EqualizerSupportedControls::kSupportsTypePeak |
                              fhasp::EqualizerSupportedControls::kSupportsTypeNotch |
                              fhasp::EqualizerSupportedControls::kSupportsTypeLowCut |
                              fhasp::EqualizerSupportedControls::kSupportsTypeHighCut |
                              fhasp::EqualizerSupportedControls::kSupportsTypeLowShelf |
                              fhasp::EqualizerSupportedControls::kSupportsTypeHighShelf,
        .can_disable_bands = true,
        .min_frequency = 0,
        .max_frequency = 24000,
        .max_q = 1000.0f,
        .min_gain_db = -60.0f,
        .max_gain_db = 60.0f,
    }}),
    .description = "Generic Equalizer element with all capabilities",
    .can_stop = true,
    .can_bypass = true,
}};
const fhasp::Element kGainElement{{
    .id = kGainElementId,
    .type = fhasp::ElementType::kGain,
    .type_specific = fhasp::TypeSpecificElement::WithGain({{
        .type = fhasp::GainType::kDecibels,
        .domain = fhasp::GainDomain::kDigital,
        .min_gain = -24.0f,
        .max_gain = 24.0f,
        .min_gain_step = 0.0f,
    }}),
    .description = "Generic Gain element with all capabilities",
    .can_stop = true,
    .can_bypass = true,
}};
const fhasp::Element kVendorSpecificElement{{
    .id = kVendorSpecificElementId,
    .type = fhasp::ElementType::kVendorSpecific,
    .type_specific = fhasp::TypeSpecificElement::WithVendorSpecific({}),
    .description = "Generic VendorSpecific element",
    .can_stop = true,
    .can_bypass = true,
}};
const fhasp::Element kElementNoId{{
    .type = fhasp::ElementType::kAutomaticGainControl,
}};
const fhasp::Element kElementNoType{{
    .id = kBadElementId,
}};
const fhasp::Element kElementWithoutRequiredTypeSpecific{{
    .id = kBadElementId,
    .type = fhasp::ElementType::kEndpoint,
}};
const fhasp::Element kElementWrongTypeSpecific{{
    .id = kBadElementId,
    .type = fhasp::ElementType::kDynamics,
    .type_specific = fhasp::TypeSpecificElement::WithEndpoint({{
        .type = fhasp::EndpointType::kDaiInterconnect,
        .plug_detect_capabilities = fhasp::PlugDetectCapabilities::kCanAsyncNotify,
    }}),
}};
const fhasp::Element kElementEmptyDescription{{
    .id = kBadElementId,
    .type = fhasp::ElementType::kAutomaticGainControl,
    .description = "",
}};
const fhasp::Element kElementCannotStop{{
    .id = kBadElementId,
    .type = fhasp::ElementType::kAutomaticGainControl,
    .can_stop = false,
}};
const fhasp::Element kElementCannotBypass{{
    .id = kBadElementId,
    .type = fhasp::ElementType::kAutomaticGainControl,
    .can_bypass = false,
}};

// Collections of Elements
const std::vector<fhasp::Element> kElements{kDaiEndpointElement, kAgcElement, kDynamicsElement,
                                            kRingBufferElement};
const std::vector<fhasp::Element> kEmptyElements{};
const std::vector<fhasp::Element> kElementsDuplicateId{kDaiEndpointElement, kDaiEndpointElement};
const std::vector<fhasp::Element> kElementsWithNoId{
    kDaiEndpointElement, kAgcElement, kDynamicsElement, kRingBufferElement, kElementNoId};
const std::vector<fhasp::Element> kElementsWithNoType{
    kDaiEndpointElement, kAgcElement, kDynamicsElement, kRingBufferElement, kElementNoType};
const std::vector<fhasp::Element> kElementsWithNoRequiredTypeSpecific{
    kDaiEndpointElement, kAgcElement, kDynamicsElement, kRingBufferElement,
    kElementWithoutRequiredTypeSpecific};
const std::vector<fhasp::Element> kElementsWithWrongTypeSpecific{
    kDaiEndpointElement, kAgcElement, kDynamicsElement, kRingBufferElement,
    kElementWrongTypeSpecific};
const std::vector<fhasp::Element> kElementsWithEmptyDescription{
    kDaiEndpointElement, kAgcElement, kDynamicsElement, kRingBufferElement,
    kElementEmptyDescription};

const std::unordered_map<ElementId, ElementRecord> kEmptyElementMap{};

// ElementStates
const fhasp::ElementState kGenericElementState{{
    // .type_specific is unspecified
    // .enabled (deprecated) is unspecified
    .latency = fhasp::Latency::WithLatencyFrames(16),
    .vendor_specific_data = {{8, 7, 6, 5, 4, 3, 2, 1, 0}},
    .started = true,
    .bypassed = false,
}};
const fhasp::ElementState kDynamicsElementState{{
    .type_specific = fhasp::TypeSpecificElementState::WithDynamics({{
        .band_states = {{
            {{
                .id = 1,
                .min_frequency = 0,
                .max_frequency = 24000,
                .threshold_db = -10.0f,
                .threshold_type = fhasp::ThresholdType::kAbove,
                .ratio = 10.0f,
                .knee_width_db = 1.0f,
                .attack = ZX_USEC(1),
                .release = ZX_USEC(10),
                .output_gain_db = -1.0f,
                .input_gain_db = 0.0f,
                .level_type = fhasp::LevelType::kPeak,
                .lookahead = ZX_USEC(10),
                .linked_channels = true,
            }},
            {{
                .id = 2,
                .min_frequency = 220,
                .max_frequency = 222,
                .threshold_db = -20.0f,
                .threshold_type = fhasp::ThresholdType::kBelow,
                .ratio = 20.0f,
            }},
        }},
    }}),
    .started = true,
}};
const fhasp::ElementState kEndpointElementState{{
    .type_specific = fhasp::TypeSpecificElementState::WithEndpoint({{
        .plug_state = fhasp::PlugState{{
            .plugged = true,
            .plug_state_time = 0,
        }},
    }}),
    .started = true,
}};
const fhasp::ElementState kEqualizerElementState{{
    .type_specific = fhasp::TypeSpecificElementState::WithEqualizer({{
        .band_states = {{
            {{
                .id = 1,
                .type = fhasp::EqualizerBandType::kPeak,
                .frequency = 110,
                .q = 1.0f,
                .gain_db = 1.0f,
                .enabled = true,
            }},
            {{
                .id = 2,
                .type = fhasp::EqualizerBandType::kNotch,
                .frequency = 220,
                .q = 2.0f,
                .enabled = true,
            }},
            {{
                .id = 3,
                .type = fhasp::EqualizerBandType::kLowCut,
                .frequency = 330,
                .q = 3.0f,
                .enabled = true,
            }},
            {{
                .id = 4,
                .type = fhasp::EqualizerBandType::kHighCut,
                .frequency = 440,
                .q = 4.0f,
                .enabled = true,
            }},
            {{
                .id = 5,
                .type = fhasp::EqualizerBandType::kLowShelf,
                .frequency = 550,
                .q = 5.0f,
                .gain_db = 5.0f,
                .enabled = true,
            }},
            {{
                .id = 6,
                .type = fhasp::EqualizerBandType::kHighShelf,
                .frequency = 660,
                .q = 6.0f,
                .gain_db = 6.0f,
                .enabled = true,
            }},
        }},
    }}),
    .started = true,
}};
const fhasp::ElementState kGainElementState{{
    .type_specific = fhasp::TypeSpecificElementState::WithGain({{.gain = 0.0f}}),
    .started = true,
}};
const fhasp::ElementState kVendorSpecificElementState{{
    .type_specific = fhasp::TypeSpecificElementState::WithVendorSpecific({}),
    .vendor_specific_data = {{0, 1, 2, 3, 4, 5, 6, 7, 8}},
    .started = true,
}};
const fhasp::ElementState kElementStateStopped{{
    .started = false,
}};
const fhasp::ElementState kElementStateBypassed{{
    .started = true,
    .bypassed = true,
}};
const fhasp::ElementState kElementStateEmpty{
    // .started (required) is unspecified
};

// Topology ids
constexpr TopologyId kTopologyDaiAgcDynRbId = 0;
constexpr TopologyId kTopologyDaiRbId = 10;
constexpr TopologyId kTopologyRbDaiId = 7;
constexpr TopologyId kBadTopologyId = 42;

// EdgePairs
const fhasp::EdgePair kEdgeDaiAgc{{kDaiEndpointElementId, kAgcElementId}};
const fhasp::EdgePair kEdgeAgcDyn{{kAgcElementId, kDynamicsElementId}};
const fhasp::EdgePair kEdgeDynRb{{kDynamicsElementId, kRingBufferEndpointElementId}};
const fhasp::EdgePair kEdgeDaiRb{{kDaiEndpointElementId, kRingBufferEndpointElementId}};
const fhasp::EdgePair kEdgeRbDai{{kRingBufferEndpointElementId, kDaiEndpointElementId}};
const fhasp::EdgePair kEdgeToSelf{{kRingBufferEndpointElementId, kRingBufferEndpointElementId}};
const fhasp::EdgePair kEdgeUnknownId{{kRingBufferEndpointElementId, kBadElementId}};

// Topologies
const fhasp::Topology kTopologyDaiAgcDynRb{{
    .id = kTopologyDaiAgcDynRbId,
    .processing_elements_edge_pairs = {{kEdgeDaiAgc, kEdgeAgcDyn, kEdgeDynRb}},
}};
const fhasp::Topology kTopologyDaiRb{{
    .id = kTopologyDaiRbId,
    .processing_elements_edge_pairs = {{kEdgeDaiRb}},
}};
const fhasp::Topology kTopologyRbDai{{
    .id = kTopologyRbDaiId,
    .processing_elements_edge_pairs = {{kEdgeRbDai}},
}};
const fhasp::Topology kTopologyMissingId{{
    .processing_elements_edge_pairs = {{kEdgeDaiRb}},
}};
const fhasp::Topology kTopologyMissingEdgePairs{{
    .id = kBadTopologyId,
}};
const fhasp::Topology kTopologyEmptyEdgePairs{{
    .id = kBadTopologyId,
    .processing_elements_edge_pairs = {{}},
}};
const fhasp::Topology kTopologyUnknownElementId{{
    .id = kBadTopologyId,
    .processing_elements_edge_pairs = {{kEdgeDaiAgc, kEdgeAgcDyn, kEdgeDynRb, kEdgeUnknownId}},
}};
const fhasp::Topology kTopologyEdgePairLoop{{
    .id = kBadTopologyId,
    .processing_elements_edge_pairs = {{kEdgeDaiAgc, kEdgeAgcDyn, kEdgeDynRb, kEdgeToSelf}},
}};
const fhasp::Topology kTopologyTerminalNotEndpoint{{
    .id = kBadTopologyId,
    .processing_elements_edge_pairs = {{kEdgeRbDai, kEdgeDaiAgc, kEdgeAgcDyn}},
}};

// Collections of topologies
const std::vector<fhasp::Topology> kTopologies{kTopologyDaiAgcDynRb, kTopologyDaiRb,
                                               kTopologyRbDai};
const std::vector<fhasp::Topology> kEmptyTopologies{};
const std::vector<fhasp::Topology> kTopologiesWithDuplicateId{kTopologyDaiAgcDynRb,
                                                              kTopologyDaiAgcDynRb};
const std::vector<fhasp::Topology> kTopologiesWithoutAllElements{kTopologyDaiRb};
const std::vector<fhasp::Topology> kTopologiesWithMissingId{kTopologyDaiAgcDynRb,
                                                            kTopologyMissingId};
const std::vector<fhasp::Topology> kTopologiesWithMissingEdgePairs{kTopologyDaiAgcDynRb,
                                                                   kTopologyMissingEdgePairs};
const std::vector<fhasp::Topology> kTopologiesWithEmptyEdgePairs{kTopologyDaiAgcDynRb,
                                                                 kTopologyEmptyEdgePairs};
const std::vector<fhasp::Topology> kTopologiesWithUnknownElementId{kTopologyDaiAgcDynRb,
                                                                   kTopologyUnknownElementId};
const std::vector<fhasp::Topology> kTopologiesWithLoop{kTopologyDaiAgcDynRb, kTopologyEdgePairLoop};
const std::vector<fhasp::Topology> kTopologiesWithTerminalNotEndpoint{kTopologyDaiAgcDynRb,
                                                                      kTopologyTerminalNotEndpoint};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_SIGNAL_PROCESSING_UTILS_UNITTEST_H_
