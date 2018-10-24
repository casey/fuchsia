// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#pragma once

#define T931_GPIO_BASE                  0xff634400
#define T931_GPIO_LENGTH                0x400
#define T931_GPIO_A0_BASE               0xff800000
#define T931_GPIO_AO_LENGTH             0x1000
#define T931_GPIO_INTERRUPT_BASE        0xffd00000
#define T931_GPIO_INTERRUPT_LENGTH      0x10000
#define T931_I2C_AOBUS_BASE             (T931_GPIO_A0_BASE + 0x5000)
#define T931_CBUS_BASE                  0xffd00000
#define T931_CBUS_LENGTH                0x100000
#define T931_I2C0_BASE                  (T931_CBUS_BASE + 0x1f000)
#define T931_I2C1_BASE                  (T931_CBUS_BASE + 0x1e000)
#define T931_I2C2_BASE                  (T931_CBUS_BASE + 0x1d000)
#define T931_I2C3_BASE                  (T931_CBUS_BASE + 0x1c000)

#define T931_HIU_BASE                   0xff63c000
#define T931_HIU_LENGTH                 0x2000

#define T931_MSR_CLK_BASE               0xffd18000
#define T931_MSR_CLK_LENGTH             0x1000

// MIPI CSI & Adapter
#define T931_CSI_PHY0_BASE              0xff650000
#define T931_CSI_PHY0_LENGTH            0x2000
#define T931_APHY_BASE                  0xff63c300
#define T931_APHY_LENGTH                0x100
#define T931_CSI_HOST0_BASE             0xff654000
#define T931_CSI_HOST0_LENGTH           0x100
#define T931_MIPI_ADAPTER_BASE          0xff650000
#define T931_MIPI_ADAPTER_LENGTH        0x6000

#define T931_USB0_BASE                  0xff500000
#define T931_USB0_LENGTH                0x100000

#define T931_USBPHY21_BASE              0xff63a000
#define T931_USBPHY21_LENGTH            0x2000

#define T931_SD_EMMC_C_BASE             0xffE07000
#define T931_SD_EMMC_C_LENGTH           0x2000

// IRQs
#define T931_GPIO_IRQ_0                 96
#define T931_GPIO_IRQ_1                 97
#define T931_GPIO_IRQ_2                 98
#define T931_GPIO_IRQ_3                 99
#define T931_GPIO_IRQ_4                 100
#define T931_GPIO_IRQ_5                 101
#define T931_GPIO_IRQ_6                 102
#define T931_GPIO_IRQ_7                 103
#define T931_I2C0_IRQ                   53
#define T931_I2C1_IRQ                   246
#define T931_I2C2_IRQ                   247
#define T931_I2C3_IRQ                   71
#define T931_I2C_AO_0_IRQ               227
#define T931_USB0_IRQ                   62
#define T931_SD_EMMC_C_IRQ              223

// Alternate Functions for EMMC
#define T931_EMMC_D0                    T931_GPIOBOOT(0)
#define T931_EMMC_D0_FN                 1
#define T931_EMMC_D1                    T931_GPIOBOOT(1)
#define T931_EMMC_D1_FN                 1
#define T931_EMMC_D2                    T931_GPIOBOOT(2)
#define T931_EMMC_D2_FN                 1
#define T931_EMMC_D3                    T931_GPIOBOOT(3)
#define T931_EMMC_D3_FN                 1
#define T931_EMMC_D4                    T931_GPIOBOOT(4)
#define T931_EMMC_D4_FN                 1
#define T931_EMMC_D5                    T931_GPIOBOOT(5)
#define T931_EMMC_D5_FN                 1
#define T931_EMMC_D6                    T931_GPIOBOOT(6)
#define T931_EMMC_D6_FN                 1
#define T931_EMMC_D7                    T931_GPIOBOOT(7)
#define T931_EMMC_D7_FN                 1
#define T931_EMMC_CLK                   T931_GPIOBOOT(8)
#define T931_EMMC_CLK_FN                1
#define T931_EMMC_RST                   T931_GPIOBOOT(9)
#define T931_EMMC_RST_FN                1
#define T931_EMMC_CMD                   T931_GPIOBOOT(10)
#define T931_EMMC_CMD_FN                1
#define T931_EMMC_DS                    T931_GPIOBOOT(15)
#define T931_EMMC_DS_FN                 1
