/*
 *  Copyright (c) 2016, The OpenThread Authors.
 *  All rights reserved.
 *
 *  Redistribution and use in source and binary forms, with or without
 *  modification, are permitted provided that the following conditions are met:
 *  1. Redistributions of source code must retain the above copyright
 *     notice, this list of conditions and the following disclaimer.
 *  2. Redistributions in binary form must reproduce the above copyright
 *     notice, this list of conditions and the following disclaimer in the
 *     documentation and/or other materials provided with the distribution.
 *  3. Neither the name of the copyright holder nor the
 *     names of its contributors may be used to endorse or promote products
 *     derived from this software without specific prior written permission.
 *
 *  THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
 *  AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
 *  IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
 *  ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
 *  LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
 *  CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
 *  SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
 *  INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
 *  CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
 *  ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
 *  POSSIBILITY OF SUCH DAMAGE.
 */

/**
 * @file
 * @brief
 *   This file includes the platform-specific initializers.
 */

#include <openthread/tasklet.h>

#include "alarm.h"
#include "misc.h"
#include "openthread-system.h"

void platformSimInit(void);
#ifdef OPENTHREAD_231010
extern "C" void platformRadioInit(const otPlatformConfig *a_platform_config);
#endif
#ifdef OPENTHREAD_240214
extern "C" void platformRadioInit(const char *aUrl);
#endif

extern "C" void platformRadioDeinit();
void platformRandomInit(void);
void platformAlarmInit(uint32_t a_speed_up_factor);

#ifdef OPENTHREAD_240214
const char *radio_url_string =
    "spinel+vendor+spi:///dev/class/ot-radio/000?baudrate=115200&no-reset&enable-coex";
#endif

void otSysInit(otPlatformConfig *a_platform_config) {
#ifdef OPENTHREAD_231010
  platformRadioInit(a_platform_config);
#endif
#ifdef OPENTHREAD_240214
  platformRadioInit(radio_url_string);
#endif
  platformRandomInit();
}

void otSysDeinit(void) {
#if OPENTHREAD_POSIX_VIRTUAL_TIME
  virtualTimeDeinit();
#endif
  platformRadioDeinit();
#if OPENTHREAD_CONFIG_PLATFORM_NETIF_ENABLE
  platformNetifDeinit();
#endif
}
