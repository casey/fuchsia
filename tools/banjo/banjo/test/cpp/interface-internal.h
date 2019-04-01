// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// WARNING: THIS FILE IS MACHINE GENERATED. DO NOT EDIT.
// Generated from the banjo.examples.interface banjo file

#pragma once

#include <type_traits>

namespace ddk {
namespace internal {

DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(has_baker_protocol_register, BakerRegister,
        void (C::*)(const cookie_maker_protocol_t* intf));

DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(has_baker_protocol_de_register, BakerDeRegister,
        void (C::*)());


template <typename D>
constexpr void CheckBakerProtocolSubclass() {
    static_assert(internal::has_baker_protocol_register<D>::value,
        "BakerProtocol subclasses must implement "
        "void BakerRegister(const cookie_maker_protocol_t* intf);");

    static_assert(internal::has_baker_protocol_de_register<D>::value,
        "BakerProtocol subclasses must implement "
        "void BakerDeRegister();");

}

DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(has_cookie_maker_protocol_prep, CookieMakerPrep,
        void (C::*)(cookie_kind_t cookie, cookie_maker_prep_callback callback, void* cookie));

DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(has_cookie_maker_protocol_bake, CookieMakerBake,
        void (C::*)(uint64_t token, zx_time_t time, cookie_maker_bake_callback callback, void* cookie));

DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(has_cookie_maker_protocol_deliver, CookieMakerDeliver,
        zx_status_t (C::*)(uint64_t token));


template <typename D>
constexpr void CheckCookieMakerProtocolSubclass() {
    static_assert(internal::has_cookie_maker_protocol_prep<D>::value,
        "CookieMakerProtocol subclasses must implement "
        "void CookieMakerPrep(cookie_kind_t cookie, cookie_maker_prep_callback callback, void* cookie);");

    static_assert(internal::has_cookie_maker_protocol_bake<D>::value,
        "CookieMakerProtocol subclasses must implement "
        "void CookieMakerBake(uint64_t token, zx_time_t time, cookie_maker_bake_callback callback, void* cookie);");

    static_assert(internal::has_cookie_maker_protocol_deliver<D>::value,
        "CookieMakerProtocol subclasses must implement "
        "zx_status_t CookieMakerDeliver(uint64_t token);");

}

} // namespace internal
} // namespace ddk
