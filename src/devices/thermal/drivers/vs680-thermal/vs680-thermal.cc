// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vs680-thermal.h"

#include <lib/device-protocol/pdev.h>
#include <lib/zx/clock.h>

#include <ddk/binding.h>
#include <ddk/debug.h>
#include <ddk/platform-defs.h>
#include <ddktl/fidl.h>
#include <ddktl/protocol/composite.h>
#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>

#include "vs680-thermal-reg.h"

namespace {

using ::llcpp::fuchsia::hardware::thermal::OperatingPoint;
using ::llcpp::fuchsia::hardware::thermal::OperatingPointEntry;

enum {
  FRAGMENT_PDEV = 0,
  FRAGMENT_CPU_CLOCK,
  FRAGMENT_COUNT,
};

constexpr OperatingPoint kOperatingPoints = {
    .opp =
        fidl::Array<OperatingPointEntry, ::llcpp::fuchsia::hardware::thermal::MAX_DVFS_OPPS>{
            // TODO(bradenkell): This is the initial CPU frequency coming out of the bootloader. Add
            //                   the other operating points when we have more information.
            OperatingPointEntry{.freq_hz = 1'800'000'000, .volt_uv = 800'000},
        },
    .latency = 0,
    .count = 1,
};

}  // namespace

namespace thermal {

zx_status_t Vs680Thermal::Create(void* ctx, zx_device_t* parent) {
  ddk::CompositeProtocolClient composite(parent);
  if (!composite.is_valid()) {
    zxlogf(ERROR, "%s: Failed to get composite protocol\n", __func__);
    return ZX_ERR_NO_RESOURCES;
  }

  zx_device_t* fragments[FRAGMENT_COUNT] = {};
  size_t fragment_count = 0;
  composite.GetFragments(fragments, FRAGMENT_COUNT, &fragment_count);
  if (fragment_count < FRAGMENT_COUNT) {
    zxlogf(ERROR, "%s: Failed to get fragments\n", __func__);
    return ZX_ERR_NO_RESOURCES;
  }

  ddk::PDev pdev(fragments[FRAGMENT_PDEV]);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "%s: Failed to get platform device protocol\n", __func__);
    return ZX_ERR_NO_RESOURCES;
  }

  std::optional<ddk::MmioBuffer> mmio;
  zx_status_t status = pdev.MapMmio(0, &mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Failed to map MMIO: %d\n", __func__, status);
    return status;
  }

  zx::interrupt interrupt;
  if ((status = pdev.GetInterrupt(0, &interrupt)) != ZX_OK) {
    zxlogf(ERROR, "%s: Failed to get interrupt: %d\n", __func__, status);
    return status;
  }

  ddk::ClockProtocolClient cpu_clock(fragments[FRAGMENT_CPU_CLOCK]);
  if (!cpu_clock.is_valid()) {
    zxlogf(ERROR, "%s: Failed to get clock protocol\n", __func__);
    return ZX_ERR_NO_RESOURCES;
  }

  fbl::AllocChecker ac;
  auto device = fbl::make_unique_checked<Vs680Thermal>(&ac, parent, *std::move(mmio),
                                                       std::move(interrupt), cpu_clock);
  if (!ac.check()) {
    zxlogf(ERROR, "%s: Failed to allocate device memory\n", __func__);
    return ZX_ERR_NO_MEMORY;
  }

  if ((status = device->Init()) != ZX_OK) {
    return status;
  }

  if ((status = device->DdkAdd("vs680-thermal")) != ZX_OK) {
    zxlogf(ERROR, "%s: DdkAdd failed: %d\n", __func__, status);
    // Init() started the interrupt thread, call DdkRelease to stop it and destroy the device
    // object.
    device->DdkRelease();
  }

  __UNUSED auto* _ = device.release();
  return status;
}

void Vs680Thermal::DdkRelease() {
  interrupt_.destroy();
  thrd_join(thread_, nullptr);
  delete this;
}

zx_status_t Vs680Thermal::DdkMessage(fidl_msg_t* msg, fidl_txn_t* txn) {
  DdkTransaction transaction(txn);
  llcpp::fuchsia::hardware::thermal::Device::Dispatch(this, msg, &transaction);
  return transaction.Status();
}

zx_status_t Vs680Thermal::Init() {
  TsenStatus::Get().ReadFrom(&mmio_).set_int_en(0).WriteTo(&mmio_);

  zx_status_t status = SetOperatingPoint(kOperatingPoints.count - 1);
  if (status != ZX_OK) {
    return status;
  }

  auto cb = [](void* arg) { return reinterpret_cast<Vs680Thermal*>(arg)->TemperatureThread(); };
  if (thrd_create_with_name(&thread_, cb, this, "vs680-thermal-thread") != thrd_success) {
    zxlogf(ERROR, "%s: Failed to create IRQ thread\n", __func__);
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

void Vs680Thermal::GetInfo(GetInfoCompleter::Sync completer) {
  completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
}

void Vs680Thermal::GetDeviceInfo(GetDeviceInfoCompleter::Sync completer) {
  // TODO(bradenkell): Implement GetDeviceInfo.
  completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
}

void Vs680Thermal::GetDvfsInfo(PowerDomain power_domain, GetDvfsInfoCompleter::Sync completer) {
  if (power_domain == PowerDomain::BIG_CLUSTER_POWER_DOMAIN) {
    OperatingPoint operating_points_copy = kOperatingPoints;
    completer.Reply(ZX_OK, fidl::unowned_ptr(&operating_points_copy));
  } else {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
  }
}

void Vs680Thermal::GetTemperatureCelsius(GetTemperatureCelsiusCompleter::Sync completer) {
  completer.Reply(ZX_OK, static_cast<float>(temperature_millicelsius_) / 1000.0f);
}

void Vs680Thermal::GetStateChangeEvent(GetStateChangeEventCompleter::Sync completer) {
  completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
}

void Vs680Thermal::GetStateChangePort(GetStateChangePortCompleter::Sync completer) {
  // TODO(bradenkell): Implement GetStateChangePort.
  completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
}

void Vs680Thermal::SetTripCelsius(uint32_t id, float temp,
                                  SetTripCelsiusCompleter::Sync completer) {
  // TODO(bradenkell): Implement SetTripCelsius.
  completer.Reply(ZX_ERR_NOT_SUPPORTED);
}

void Vs680Thermal::GetDvfsOperatingPoint(PowerDomain power_domain,
                                         GetDvfsOperatingPointCompleter::Sync completer) {
  if (power_domain == PowerDomain::BIG_CLUSTER_POWER_DOMAIN) {
    completer.Reply(ZX_OK, operating_point_);
  } else {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, 0);
  }
}

void Vs680Thermal::SetDvfsOperatingPoint(uint16_t op_idx, PowerDomain power_domain,
                                         SetDvfsOperatingPointCompleter::Sync completer) {
  if (power_domain == PowerDomain::BIG_CLUSTER_POWER_DOMAIN) {
    completer.Reply(SetOperatingPoint(op_idx));
  } else {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
  }
}

void Vs680Thermal::GetFanLevel(GetFanLevelCompleter::Sync completer) {
  completer.Reply(ZX_ERR_NOT_SUPPORTED, 0);
}

void Vs680Thermal::SetFanLevel(uint32_t fan_level, SetFanLevelCompleter::Sync completer) {
  completer.Reply(ZX_ERR_NOT_SUPPORTED);
}

zx_status_t Vs680Thermal::SetOperatingPoint(uint16_t op_idx) {
  if (op_idx >= kOperatingPoints.count) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // TODO(bradenkell): Change VCPU if required by the chosen operating point.

  zx_status_t status = cpu_clock_.SetRate(kOperatingPoints.opp[op_idx].freq_hz);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Failed to set CPU clock rate to %u: %d\n", __func__,
           kOperatingPoints.opp[op_idx].freq_hz, status);
    return status;
  }

  operating_point_ = op_idx;
  return ZX_OK;
}

int Vs680Thermal::TemperatureThread() {
  for (;;) {
    const zx::time next_poll_time = zx::clock::get_monotonic() + poll_interval_;

    TsenStatus::Get().ReadFrom(&mmio_).set_int_en(1).WriteTo(&mmio_);
    TsenCtrl::Get().ReadFrom(&mmio_).set_ena(1).WriteTo(&mmio_).set_clk_en(1).WriteTo(&mmio_);

    zx_status_t status = interrupt_.wait(nullptr);
    if (status == ZX_ERR_CANCELED) {
      break;
    } else if (status != ZX_OK) {
      zxlogf(INFO, "%s: Interrupt wait returned %d\n", __func__, status);
      break;
    }

    const int64_t sensor_data = TsenData::Get().ReadFrom(&mmio_).data();

    TsenStatus::Get().ReadFrom(&mmio_).set_data_rdy(0).WriteTo(&mmio_);
    TsenCtrl::Get().ReadFrom(&mmio_).set_ena(0).WriteTo(&mmio_).set_clk_en(0).WriteTo(&mmio_);

    int64_t temperature = (18439 * sensor_data) / 1000;
    temperature = ((80705 - temperature) * sensor_data) / 1000;
    temperature = ((185010 - temperature) * sensor_data) / 1000;
    temperature = ((328430 - temperature) * sensor_data) / 1000;
    temperature -= 48690;

    temperature_millicelsius_ = temperature;

    const zx::time current_time = zx::clock::get_monotonic();
    if (next_poll_time > current_time) {
      zx::nanosleep(zx::deadline_after(next_poll_time - current_time));
    }
  }

  return thrd_success;
}

}  // namespace thermal

static constexpr zx_driver_ops_t vs680_thermal_driver_ops = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = thermal::Vs680Thermal::Create;
  return ops;
}();

// clang-format off
ZIRCON_DRIVER_BEGIN(vs680_thermal, vs680_thermal_driver_ops, "zircon", "0.1", 3)
  BI_ABORT_IF(NE, BIND_PROTOCOL, ZX_PROTOCOL_COMPOSITE),
  BI_ABORT_IF(NE, BIND_PLATFORM_DEV_VID, PDEV_VID_SYNAPTICS),
  BI_MATCH_IF(EQ, BIND_PLATFORM_DEV_DID, PDEV_DID_VS680_THERMAL),
ZIRCON_DRIVER_END(vs680_thermal)
// clang-format on
