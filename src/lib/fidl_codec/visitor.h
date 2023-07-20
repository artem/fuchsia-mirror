// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_FIDL_CODEC_VISITOR_H_
#define SRC_LIB_FIDL_CODEC_VISITOR_H_

#include "src/lib/fidl_codec/wire_object.h"
#include "src/lib/fidl_codec/wire_types.h"

namespace fidl_codec {

// Superclass for implementing visitors for Values. Note that the whole class is protected. To use a
// visitor, use the Visit method on the Value object you want to visit.
class Visitor {
 protected:
  virtual void VisitValue(const Value* node, const Type* for_type) {}
  virtual void VisitInvalidValue(const InvalidValue* node, const Type* for_type) {
    VisitValue(node, for_type);
  }
  virtual void VisitEmptyPayloadValue(const EmptyPayloadValue* node, const Type* for_type) {
    VisitValue(node, for_type);
  }
  virtual void VisitNullValue(const NullValue* node, const Type* for_type) {
    VisitValue(node, for_type);
  }
  virtual void VisitRawValue(const RawValue* node, const Type* for_type) {
    VisitValue(node, for_type);
  }
  virtual void VisitBoolValue(const BoolValue* node, const Type* for_type) {
    VisitValue(node, for_type);
  }
  virtual void VisitIntegerValue(const IntegerValue* node, const Type* for_type) {
    VisitValue(node, for_type);
  }
  virtual void VisitActualAndRequestedValue(const ActualAndRequestedValue* node,
                                            const Type* for_type) {
    VisitValue(node, for_type);
  }
  virtual void VisitDoubleValue(const DoubleValue* node, const Type* for_type) {
    VisitValue(node, for_type);
  }
  virtual void VisitStringValue(const StringValue* node, const Type* for_type) {
    VisitValue(node, for_type);
  }
  virtual void VisitHandleValue(const HandleValue* node, const Type* for_type) {
    VisitValue(node, for_type);
  }
  virtual void VisitUnionValue(const UnionValue* node, const Type* for_type) {
    node->value()->Visit(this, node->member().type());
  }
  virtual void VisitStructValue(const StructValue* node, const Type* for_type) {
    for (const auto& field : node->fields()) {
      field.second->Visit(this, field.first->type());
    }
  }
  virtual void VisitVectorValue(const VectorValue* node, const Type* for_type) {
    FX_DCHECK(for_type != nullptr);
    const Type* component_type = for_type->GetComponentType();
    FX_DCHECK(component_type != nullptr);
    for (const auto& value : node->values()) {
      value->Visit(this, component_type);
    }
  }
  virtual void VisitTableValue(const TableValue* node, const Type* for_type) {
    for (const auto& member : node->members()) {
      member.second->Visit(this, member.first->type());
    }
  }
  virtual void VisitFidlMessageValue(const FidlMessageValue* node, const Type* for_type) {
    if (node->decoded_request() != nullptr) {
      node->decoded_request()->Visit(this, nullptr);
    }
    if (node->decoded_response() != nullptr) {
      node->decoded_response()->Visit(this, nullptr);
    }
  }

  friend class Value;
  friend class InvalidValue;
  friend class EmptyPayloadValue;
  friend class NullValue;
  friend class RawValue;
  friend class BoolValue;
  friend class IntegerValue;
  friend class ActualAndRequestedValue;
  friend class DoubleValue;
  friend class StringValue;
  friend class HandleValue;
  friend class UnionValue;
  friend class StructValue;
  friend class VectorValue;
  friend class TableValue;
  friend class FidlMessageValue;
};

}  // namespace fidl_codec

#endif  // SRC_LIB_FIDL_CODEC_VISITOR_H_
