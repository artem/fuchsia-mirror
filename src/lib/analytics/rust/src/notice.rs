// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub const FULL_NOTICE: &str = "Welcome to Fuchsia! - https://fuchsia.dev

Fuchsia developer tools use Google Analytics to report feature usage
statistics and basic crash reports. Google may examine the collected data
in aggregate to help improve these tools, other Fuchsia tools, and the
Fuchsia SDK.

Analytics are not sent on this very first run. To disable reporting, type

    ffx config analytics disable

To display the current setting, type

    ffx config analytics show

If you opt out of analytics, an opt-out event will be sent, and then no
further information will be sent by the developer tools to Google.

By using Fuchsia developer tools, you agree to the Google Terms of Service.
Note: The Google Privacy Policy describes how data is handled in your use of
this service.

Read about the data we send:
https://fuchsia.dev/fuchsia-src/contribute/governance/policy/analytics_collected_fuchsia_tools?hl=en

See Google's privacy policy:
https://policies.google.com/privacy
";

pub const BRIEF_NOTICE: &str = "Welcome to Fuchsia!

As part of the Fuchsia developer tools, this tool uses Google Analytics to
report feature usage statistics and basic crash reports. Google may examine the
collected data in aggregate to help improve the developer tools, other
Fuchsia tools, and the Fuchsia SDK.

To disable reporting, type

    ffx config analytics disable

To display the current setting, type

    ffx config analytics show

If you opt out of analytics, an opt-out event will be sent, and then no further
information will be sent by the developer tools to Google.

Read about the data we send :
https://fuchsia.dev/fuchsia-src/contribute/governance/policy/analytics_collected_fuchsia_tools?hl=en

See Google's privacy policy:
https://policies.google.com/privacy
";
