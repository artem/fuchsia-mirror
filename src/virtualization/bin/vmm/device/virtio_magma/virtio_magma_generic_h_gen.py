#!/usr/bin/env fuchsia-vendored-python
# Copyright 2019 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import sys


def usage():
    print('Usage:')
    print('  virtio_magma_generic_h_gen.py INPUT OUTPUT')
    print('    INPUT    json file containing the magma interface definition')
    print('    OUTPUT   destination path for the header file to generate')
    print(
        '  Example: virtio_magma_generic_h_gen.py magma.json virtio_magma_generic.h'
    )
    print('  Generates a generic "glue" class that directly translates between')
    print('  virtmagma structs and magma commands, that may be overridden.')


# License string for the top of the file.
def license():
    ret = ''
    ret += '// Copyright 2019 The Fuchsia Authors. All rights reserved.\n'
    ret += '// Use of this source code is governed by a BSD-style license that can be\n'
    ret += '// found in the LICENSE file.\n'
    return ret


# Warning string about auto-generation
def warning():
    ret = ''
    ret += '// NOTE: DO NOT EDIT THIS FILE! It is generated automatically by:\n'
    ret += '//   //src/virtualization/bin/vmm/device/virtio_magma_generic_h_gen.py\n'
    return ret


# Guard macro that goes at the beginning/end of the header (after license).
def guards(begin):
    macro = 'SRC_VIRTUALIZATION_BIN_VMM_DEVICE_VIRTIO_MAGMA_GENERIC_H_'
    if begin:
        return '#ifndef ' + macro + '\n#define ' + macro + '\n'
    return '#endif // ' + macro


# Includes lists.
def includes():
    ret = ''
    ret += '#include <fbl/unique_fd.h>\n'
    ret += '#include "src/lib/fxl/macros.h"\n'
    ret += '#include <lib/trace/event.h>\n'
    ret += '#include <zircon/status.h>\n'
    ret += '#include "src/virtualization/bin/vmm/device/virtio_queue.h"\n'
    ret += '#include <string>\n'
    return ret


# Extract the non-"magma_" portion of the name of an export
def get_name(export):
    return export['name'][len('magma_'):]


# Determine if the argument type needs special handling and a generic method can't be created for it.
def has_unsupported_argument(export):
    for argument in export['arguments']:
        if "magma_buffer_info_t" in argument['type']:
            return True
    return False


def is_response_argument(argument):
    if argument['name'][-4:] == '_out':
        assert argument['type'][-1] == '*', 'output argument not a pointer'
        # Response arguments must be pointers to 8 byte arguments, so we can pass
        # the dereferenced value over the wire in only 8 bytes.
        if argument['type'].find('magma_image_info_t') != -1:
            return False
        return True
    return False


# Generate a method that does simple validation of a virtio command struct,
# passes it on to magma, and populates the corresponding response struct.
# Also generate an unimplemented, low level method for special case handling.
# If the low level method is not overridden, the other method will be used.
def generate_generic_method(method, is_internal):
    name = get_name(method)
    ret = ''
    ret += '  virtual zx_status_t Handle_' + name + '(\n'
    ret += '    VirtioDescriptor* request_desc, VirtioDescriptor* response_desc, uint32_t* used_out) \n'
    ret += '        { return ZX_ERR_NOT_SUPPORTED; }\n'
    ret += '\n'
    ret += '  virtual zx_status_t Handle_' + name + '(\n'
    ret += '    const virtio_magma_' + name + '_ctrl_t* request,\n'
    ret += '    virtio_magma_' + name + '_resp_t* response)'
    if has_unsupported_argument(method) or is_internal:
        ret += ' { return ZX_ERR_NOT_SUPPORTED; }\n'
        return ret
    ret += ' {\n'
    ret += '    TRACE_DURATION("machina", "VirtioMagmaGeneric::Handle_' + name + '");\n'
    ret += '    FX_DCHECK(request->hdr.type == VIRTIO_MAGMA_CMD_' + name.upper(
    ) + ');\n'
    invocation_args = ''
    copy_temporaries = ''
    for argument in method['arguments']:
        invocation_args += '      '
        if is_response_argument(argument):
            # Create locals for output parameters. This is necessary due to struct packing.
            temp_name = 'temp_' + argument['name']
            ret += '    ' + argument['type'][:-1] + ' ' + temp_name + '{};\n'
            invocation_args += '&' + temp_name + ',\n'
            copy_temporaries += '    response->' + argument[
                'name'] + ' = (__typeof(response->' + argument[
                    'name'] + '))' + temp_name + ';\n'
        else:
            invocation_args += '(' + argument['type'] + ')request->' + argument[
                'name'] + ',\n'

    ret += '    '
    if method['type'] != 'void':
        ret += 'response->result_return = '
    ret += method['name'] + '(\n'
    ret += invocation_args[:-2] + ');\n'
    ret += copy_temporaries
    ret += '    response->hdr.type = VIRTIO_MAGMA_RESP_' + name.upper() + ';\n'
    ret += '    return ZX_OK;\n'
    ret += '  }\n'
    return ret


# Generate the main command switch method
def generate_handle_command(magma):
    ret = '''
    virtual zx_status_t HandleCommand(VirtioChain chain) {
    TRACE_DURATION("machina", "VirtioMagma::HandleCommand");
    VirtioDescriptor request_desc{};
    if (!chain.NextDescriptor(&request_desc)) {
      FX_LOGS(ERROR) << "Failed to read request descriptor";
      return ZX_ERR_INTERNAL;
    }
    const auto request_header = reinterpret_cast<virtio_magma_ctrl_hdr_t*>(request_desc.addr);
    const uint32_t command_type = request_header->type;
    if (!chain.HasDescriptor()) {
      FX_LOGS(ERROR) << "MAGMA command (" << command_type << ") missing response descriptor";
      return ZX_ERR_INVALID_ARGS;
    }
    VirtioDescriptor response_desc{};
    if (!chain.NextDescriptor(&response_desc)) {
      FX_LOGS(ERROR) << "Failed to read response descriptor";
      return ZX_ERR_INTERNAL;
    }
    if (!response_desc.writable) {
      FX_LOGS(ERROR) << "MAGMA command (" << command_type << ") response descriptor not writable";
      return ZX_ERR_INVALID_ARGS;
    }
    if (!device_path_.has_value()) {
      auto response_header = reinterpret_cast<virtio_magma_ctrl_hdr_t*>(response_desc.addr);
      response_header->type = VIRTIO_MAGMA_RESP_ERR_HOST_DISCONNECTED;
      chain.Return();
      return ZX_OK;
    }
    switch (command_type) {
'''
    for export in magma['exports'] + magma['virtmagma-internal']:
        name = get_name(export)
        ret += '      case VIRTIO_MAGMA_CMD_' + name.upper() + ': {\n'
        ret += '        TRACE_DURATION("magma", "' + name + '");\n'
        ret += '        auto request = *reinterpret_cast<const virtio_magma_' + name + '_ctrl_t*>(request_desc.addr);\n'
        ret += '        if (request_desc.len < sizeof(request)) {\n'
        ret += '          FX_LOGS(ERROR) << "MAGMA command (" << command_type << ") request descriptor too small";\n'
        ret += '          chain.Return();\n'
        ret += '          return ZX_ERR_INVALID_ARGS;\n'
        ret += '        }\n'
        ret += '#ifdef VIRTMAGMA_DEBUG\n'
        ret += '        FX_LOGS(INFO) << "Received MAGMA command (" << command_type << "):\\n"\\\n'
        ret += '          "  hdr = { " << virtio_magma_ctrl_type_string((virtio_magma_ctrl_type)request.hdr.type) << ", " << request.hdr.flags << " }"\\\n'
        for argument in export['arguments']:
            if argument['name'].find('_out') == -1:
                ret += '          "\\n  ' + argument[
                    'name'] + ' = " << static_cast<uint64_t>(request.' + argument[
                        'name'] + ') << ""\\\n'
        ret += '          "";\n'
        ret += '#endif // VIRTMAGMA_DEBUG\n'
        ret += '        virtio_magma_' + name + '_resp_t response{};\n'
        ret += '        if (response_desc.len < sizeof(response)) {\n'
        ret += '          FX_LOGS(ERROR) << "MAGMA command (" << command_type << ") response descriptor too small";\n'
        ret += '          chain.Return();\n'
        ret += '          return ZX_ERR_INVALID_ARGS;\n'
        ret += '        }\n'
        ret += '        uint32_t used;\n'
        ret += '        zx_status_t status = Handle_' + name + '(&request_desc, &response_desc, &used);\n'
        ret += '        if (status != ZX_ERR_NOT_SUPPORTED) {\n'
        ret += '          *chain.Used() = used;\n'
        ret += '          chain.Return();\n'
        ret += '          return status;\n'
        ret += '        }\n'
        ret += '        status = Handle_' + name + '(&request, &response);\n'
        ret += '        if (status != ZX_OK) {\n'
        ret += '          FX_LOGS(ERROR) << "Handle_' + name + ' failed (" << zx_status_get_string(status) << ")";\n'
        ret += '          chain.Return();\n'
        ret += '          return status;\n'
        ret += '        }\n'
        ret += '#ifdef VIRTMAGMA_DEBUG\n'
        ret += '        FX_LOGS(INFO) << "Sending MAGMA response:\\n"\\\n'
        ret += '          "  hdr = { " << virtio_magma_ctrl_type_string((virtio_magma_ctrl_type)response.hdr.type) << ", " << response.hdr.flags << " }"\\\n'
        for argument in export['arguments']:
            if is_response_argument(argument):
                ret += '          "\\n  ' + argument[
                    'name'] + ' = " << static_cast<uint64_t>(response.' + argument[
                        'name'] + ') << ""\\\n'
        if export['type'] != 'void':
            ret += '          "\\n  result_return = " << static_cast<int64_t>(response.result_return) << ""\\\n'
        ret += '          "";\n'
        ret += '#endif // VIRTMAGMA_DEBUG\n'
        ret += '        memcpy(response_desc.addr, &response, sizeof(response));\n'
        ret += '        *chain.Used() = sizeof(response);\n'
        ret += '      } break;\n'
    ret += '''      default: {
        FX_LOGS(ERROR) << "Unsupported MAGMA command (" << command_type << ")";
        auto response = reinterpret_cast<virtio_magma_ctrl_hdr_t*>(response_desc.addr);
        response->type = VIRTIO_MAGMA_RESP_ERR_INVALID_COMMAND;
        *chain.Used() = sizeof(*response);
      } break;
    }
    chain.Return();
    return ZX_OK;
  }'''
    return ret


def main():
    if (len(sys.argv) != 3):
        usage()
        return 2
    with open(sys.argv[1], 'r') as file:
        with open(sys.argv[2], 'w') as dest:
            magma = json.load(file)['magma-interface']
            header = license() + '\n'
            header += warning() + '\n'
            header += guards(True) + '\n'
            header += includes() + '\n'
            header += 'class VirtioMagmaGeneric {\n'
            header += ' public:\n'
            for method in magma['exports']:
                header += generate_generic_method(method, False) + '\n'
            for method in magma['virtmagma-internal']:
                name = method['name']
                if not name.startswith('magma_internal_'):
                    sys.exit("'%s' must start with 'magma_internal_'" % name)
                header += generate_generic_method(method, True) + '\n'
            header += generate_handle_command(magma) + '\n'
            header += ' protected:\n'
            header += '  std::optional<std::string> device_path_;\n'
            header += '};\n'
            header += guards(False) + '\n'
            dest.write(header)


if __name__ == '__main__':
    sys.exit(main())
