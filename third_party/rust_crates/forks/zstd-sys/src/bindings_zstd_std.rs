/* automatically generated by rust-bindgen 0.60.1 */

// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Generated by ./third_party/rust_crates/forks/zstd-sys/bindgen.sh using bindgen 0.60.1

pub const ZSTD_VERSION_MAJOR: u32 = 1;
pub const ZSTD_VERSION_MINOR: u32 = 5;
pub const ZSTD_VERSION_RELEASE: u32 = 2;
pub const ZSTD_VERSION_NUMBER: u32 = 10502;
pub const ZSTD_CLEVEL_DEFAULT: u32 = 3;
pub const ZSTD_MAGICNUMBER: u32 = 4247762216;
pub const ZSTD_MAGIC_DICTIONARY: u32 = 3962610743;
pub const ZSTD_MAGIC_SKIPPABLE_START: u32 = 407710288;
pub const ZSTD_MAGIC_SKIPPABLE_MASK: u32 = 4294967280;
pub const ZSTD_BLOCKSIZELOG_MAX: u32 = 17;
pub const ZSTD_BLOCKSIZE_MAX: u32 = 131072;
pub const ZSTD_CONTENTSIZE_UNKNOWN: i32 = -1;
pub const ZSTD_CONTENTSIZE_ERROR: i32 = -2;
extern "C" {
    #[doc = " ZSTD_versionNumber() :"]
    #[doc = "  Return runtime library version, the value is (MAJOR*100*100 + MINOR*100 + RELEASE)."]
    pub fn ZSTD_versionNumber() -> ::std::os::raw::c_uint;
}
extern "C" {
    #[doc = " ZSTD_versionString() :"]
    #[doc = "  Return runtime library version, like \"1.4.5\". Requires v1.3.0+."]
    pub fn ZSTD_versionString() -> *const ::std::os::raw::c_char;
}
extern "C" {
    #[doc = "  Simple API"]
    #[doc = "  Compresses `src` content as a single zstd compressed frame into already allocated `dst`."]
    #[doc = "  Hint : compression runs faster if `dstCapacity` >=  `ZSTD_compressBound(srcSize)`."]
    #[doc = "  @return : compressed size written into `dst` (<= `dstCapacity),"]
    #[doc = "            or an error code if it fails (which can be tested using ZSTD_isError())."]
    pub fn ZSTD_compress(
        dst: *mut ::core::ffi::c_void,
        dstCapacity: usize,
        src: *const ::core::ffi::c_void,
        srcSize: usize,
        compressionLevel: ::std::os::raw::c_int,
    ) -> usize;
}
extern "C" {
    #[doc = " ZSTD_decompress() :"]
    #[doc = "  `compressedSize` : must be the _exact_ size of some number of compressed and/or skippable frames."]
    #[doc = "  `dstCapacity` is an upper bound of originalSize to regenerate."]
    #[doc = "  If user cannot imply a maximum upper bound, it's better to use streaming mode to decompress data."]
    #[doc = "  @return : the number of bytes decompressed into `dst` (<= `dstCapacity`),"]
    #[doc = "            or an errorCode if it fails (which can be tested using ZSTD_isError())."]
    pub fn ZSTD_decompress(
        dst: *mut ::core::ffi::c_void,
        dstCapacity: usize,
        src: *const ::core::ffi::c_void,
        compressedSize: usize,
    ) -> usize;
}
extern "C" {
    pub fn ZSTD_getFrameContentSize(
        src: *const ::core::ffi::c_void,
        srcSize: usize,
    ) -> ::std::os::raw::c_ulonglong;
}
extern "C" {
    #[doc = " ZSTD_getDecompressedSize() :"]
    #[doc = "  NOTE: This function is now obsolete, in favor of ZSTD_getFrameContentSize()."]
    #[doc = "  Both functions work the same way, but ZSTD_getDecompressedSize() blends"]
    #[doc = "  \"empty\", \"unknown\" and \"error\" results to the same return value (0),"]
    #[doc = "  while ZSTD_getFrameContentSize() gives them separate return values."]
    #[doc = " @return : decompressed size of `src` frame content _if known and not empty_, 0 otherwise."]
    pub fn ZSTD_getDecompressedSize(
        src: *const ::core::ffi::c_void,
        srcSize: usize,
    ) -> ::std::os::raw::c_ulonglong;
}
extern "C" {
    #[doc = " ZSTD_findFrameCompressedSize() : Requires v1.4.0+"]
    #[doc = " `src` should point to the start of a ZSTD frame or skippable frame."]
    #[doc = " `srcSize` must be >= first frame size"]
    #[doc = " @return : the compressed size of the first frame starting at `src`,"]
    #[doc = "           suitable to pass as `srcSize` to `ZSTD_decompress` or similar,"]
    #[doc = "        or an error code if input is invalid"]
    pub fn ZSTD_findFrameCompressedSize(src: *const ::core::ffi::c_void, srcSize: usize) -> usize;
}
extern "C" {
    pub fn ZSTD_compressBound(srcSize: usize) -> usize;
}
extern "C" {
    pub fn ZSTD_isError(code: usize) -> ::std::os::raw::c_uint;
}
extern "C" {
    pub fn ZSTD_getErrorName(code: usize) -> *const ::std::os::raw::c_char;
}
extern "C" {
    pub fn ZSTD_minCLevel() -> ::std::os::raw::c_int;
}
extern "C" {
    pub fn ZSTD_maxCLevel() -> ::std::os::raw::c_int;
}
extern "C" {
    pub fn ZSTD_defaultCLevel() -> ::std::os::raw::c_int;
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ZSTD_CCtx_s {
    _unused: [u8; 0],
}
#[doc = "  Explicit context"]
pub type ZSTD_CCtx = ZSTD_CCtx_s;
extern "C" {
    pub fn ZSTD_createCCtx() -> *mut ZSTD_CCtx;
}
extern "C" {
    pub fn ZSTD_freeCCtx(cctx: *mut ZSTD_CCtx) -> usize;
}
extern "C" {
    #[doc = " ZSTD_compressCCtx() :"]
    #[doc = "  Same as ZSTD_compress(), using an explicit ZSTD_CCtx."]
    #[doc = "  Important : in order to behave similarly to `ZSTD_compress()`,"]
    #[doc = "  this function compresses at requested compression level,"]
    #[doc = "  __ignoring any other parameter__ ."]
    #[doc = "  If any advanced parameter was set using the advanced API,"]
    #[doc = "  they will all be reset. Only `compressionLevel` remains."]
    pub fn ZSTD_compressCCtx(
        cctx: *mut ZSTD_CCtx,
        dst: *mut ::core::ffi::c_void,
        dstCapacity: usize,
        src: *const ::core::ffi::c_void,
        srcSize: usize,
        compressionLevel: ::std::os::raw::c_int,
    ) -> usize;
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ZSTD_DCtx_s {
    _unused: [u8; 0],
}
pub type ZSTD_DCtx = ZSTD_DCtx_s;
extern "C" {
    pub fn ZSTD_createDCtx() -> *mut ZSTD_DCtx;
}
extern "C" {
    pub fn ZSTD_freeDCtx(dctx: *mut ZSTD_DCtx) -> usize;
}
extern "C" {
    #[doc = " ZSTD_decompressDCtx() :"]
    #[doc = "  Same as ZSTD_decompress(),"]
    #[doc = "  requires an allocated ZSTD_DCtx."]
    #[doc = "  Compatible with sticky parameters."]
    pub fn ZSTD_decompressDCtx(
        dctx: *mut ZSTD_DCtx,
        dst: *mut ::core::ffi::c_void,
        dstCapacity: usize,
        src: *const ::core::ffi::c_void,
        srcSize: usize,
    ) -> usize;
}
#[repr(u32)]
#[doc = "  Advanced compression API (Requires v1.4.0+)"]
#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub enum ZSTD_strategy {
    ZSTD_fast = 1,
    ZSTD_dfast = 2,
    ZSTD_greedy = 3,
    ZSTD_lazy = 4,
    ZSTD_lazy2 = 5,
    ZSTD_btlazy2 = 6,
    ZSTD_btopt = 7,
    ZSTD_btultra = 8,
    ZSTD_btultra2 = 9,
}
#[repr(u32)]
#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub enum ZSTD_cParameter {
    ZSTD_c_compressionLevel = 100,
    ZSTD_c_windowLog = 101,
    ZSTD_c_hashLog = 102,
    ZSTD_c_chainLog = 103,
    ZSTD_c_searchLog = 104,
    ZSTD_c_minMatch = 105,
    ZSTD_c_targetLength = 106,
    ZSTD_c_strategy = 107,
    ZSTD_c_enableLongDistanceMatching = 160,
    ZSTD_c_ldmHashLog = 161,
    ZSTD_c_ldmMinMatch = 162,
    ZSTD_c_ldmBucketSizeLog = 163,
    ZSTD_c_ldmHashRateLog = 164,
    ZSTD_c_contentSizeFlag = 200,
    ZSTD_c_checksumFlag = 201,
    ZSTD_c_dictIDFlag = 202,
    ZSTD_c_nbWorkers = 400,
    ZSTD_c_jobSize = 401,
    ZSTD_c_overlapLog = 402,
    ZSTD_c_experimentalParam1 = 500,
    ZSTD_c_experimentalParam2 = 10,
    ZSTD_c_experimentalParam3 = 1000,
    ZSTD_c_experimentalParam4 = 1001,
    ZSTD_c_experimentalParam5 = 1002,
    ZSTD_c_experimentalParam6 = 1003,
    ZSTD_c_experimentalParam7 = 1004,
    ZSTD_c_experimentalParam8 = 1005,
    ZSTD_c_experimentalParam9 = 1006,
    ZSTD_c_experimentalParam10 = 1007,
    ZSTD_c_experimentalParam11 = 1008,
    ZSTD_c_experimentalParam12 = 1009,
    ZSTD_c_experimentalParam13 = 1010,
    ZSTD_c_experimentalParam14 = 1011,
    ZSTD_c_experimentalParam15 = 1012,
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ZSTD_bounds {
    pub error: usize,
    pub lowerBound: ::std::os::raw::c_int,
    pub upperBound: ::std::os::raw::c_int,
}
extern "C" {
    #[doc = " ZSTD_cParam_getBounds() :"]
    #[doc = "  All parameters must belong to an interval with lower and upper bounds,"]
    #[doc = "  otherwise they will either trigger an error or be automatically clamped."]
    #[doc = " @return : a structure, ZSTD_bounds, which contains"]
    #[doc = "         - an error status field, which must be tested using ZSTD_isError()"]
    #[doc = "         - lower and upper bounds, both inclusive"]
    pub fn ZSTD_cParam_getBounds(cParam: ZSTD_cParameter) -> ZSTD_bounds;
}
extern "C" {
    #[doc = " ZSTD_CCtx_setParameter() :"]
    #[doc = "  Set one compression parameter, selected by enum ZSTD_cParameter."]
    #[doc = "  All parameters have valid bounds. Bounds can be queried using ZSTD_cParam_getBounds()."]
    #[doc = "  Providing a value beyond bound will either clamp it, or trigger an error (depending on parameter)."]
    #[doc = "  Setting a parameter is generally only possible during frame initialization (before starting compression)."]
    #[doc = "  Exception : when using multi-threading mode (nbWorkers >= 1),"]
    #[doc = "              the following parameters can be updated _during_ compression (within same frame):"]
    #[doc = "              => compressionLevel, hashLog, chainLog, searchLog, minMatch, targetLength and strategy."]
    #[doc = "              new parameters will be active for next job only (after a flush())."]
    #[doc = " @return : an error code (which can be tested using ZSTD_isError())."]
    pub fn ZSTD_CCtx_setParameter(
        cctx: *mut ZSTD_CCtx,
        param: ZSTD_cParameter,
        value: ::std::os::raw::c_int,
    ) -> usize;
}
extern "C" {
    #[doc = " ZSTD_CCtx_setPledgedSrcSize() :"]
    #[doc = "  Total input data size to be compressed as a single frame."]
    #[doc = "  Value will be written in frame header, unless if explicitly forbidden using ZSTD_c_contentSizeFlag."]
    #[doc = "  This value will also be controlled at end of frame, and trigger an error if not respected."]
    #[doc = " @result : 0, or an error code (which can be tested with ZSTD_isError())."]
    #[doc = "  Note 1 : pledgedSrcSize==0 actually means zero, aka an empty frame."]
    #[doc = "           In order to mean \"unknown content size\", pass constant ZSTD_CONTENTSIZE_UNKNOWN."]
    #[doc = "           ZSTD_CONTENTSIZE_UNKNOWN is default value for any new frame."]
    #[doc = "  Note 2 : pledgedSrcSize is only valid once, for the next frame."]
    #[doc = "           It's discarded at the end of the frame, and replaced by ZSTD_CONTENTSIZE_UNKNOWN."]
    #[doc = "  Note 3 : Whenever all input data is provided and consumed in a single round,"]
    #[doc = "           for example with ZSTD_compress2(),"]
    #[doc = "           or invoking immediately ZSTD_compressStream2(,,,ZSTD_e_end),"]
    #[doc = "           this value is automatically overridden by srcSize instead."]
    pub fn ZSTD_CCtx_setPledgedSrcSize(
        cctx: *mut ZSTD_CCtx,
        pledgedSrcSize: ::std::os::raw::c_ulonglong,
    ) -> usize;
}
#[repr(u32)]
#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub enum ZSTD_ResetDirective {
    ZSTD_reset_session_only = 1,
    ZSTD_reset_parameters = 2,
    ZSTD_reset_session_and_parameters = 3,
}
extern "C" {
    #[doc = " ZSTD_CCtx_reset() :"]
    #[doc = "  There are 2 different things that can be reset, independently or jointly :"]
    #[doc = "  - The session : will stop compressing current frame, and make CCtx ready to start a new one."]
    #[doc = "                  Useful after an error, or to interrupt any ongoing compression."]
    #[doc = "                  Any internal data not yet flushed is cancelled."]
    #[doc = "                  Compression parameters and dictionary remain unchanged."]
    #[doc = "                  They will be used to compress next frame."]
    #[doc = "                  Resetting session never fails."]
    #[doc = "  - The parameters : changes all parameters back to \"default\"."]
    #[doc = "                  This removes any reference to any dictionary too."]
    #[doc = "                  Parameters can only be changed between 2 sessions (i.e. no compression is currently ongoing)"]
    #[doc = "                  otherwise the reset fails, and function returns an error value (which can be tested using ZSTD_isError())"]
    #[doc = "  - Both : similar to resetting the session, followed by resetting parameters."]
    pub fn ZSTD_CCtx_reset(cctx: *mut ZSTD_CCtx, reset: ZSTD_ResetDirective) -> usize;
}
extern "C" {
    #[doc = " ZSTD_compress2() :"]
    #[doc = "  Behave the same as ZSTD_compressCCtx(), but compression parameters are set using the advanced API."]
    #[doc = "  ZSTD_compress2() always starts a new frame."]
    #[doc = "  Should cctx hold data from a previously unfinished frame, everything about it is forgotten."]
    #[doc = "  - Compression parameters are pushed into CCtx before starting compression, using ZSTD_CCtx_set*()"]
    #[doc = "  - The function is always blocking, returns when compression is completed."]
    #[doc = "  Hint : compression runs faster if `dstCapacity` >=  `ZSTD_compressBound(srcSize)`."]
    #[doc = " @return : compressed size written into `dst` (<= `dstCapacity),"]
    #[doc = "           or an error code if it fails (which can be tested using ZSTD_isError())."]
    pub fn ZSTD_compress2(
        cctx: *mut ZSTD_CCtx,
        dst: *mut ::core::ffi::c_void,
        dstCapacity: usize,
        src: *const ::core::ffi::c_void,
        srcSize: usize,
    ) -> usize;
}
#[repr(u32)]
#[doc = "  Advanced decompression API (Requires v1.4.0+)"]
#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub enum ZSTD_dParameter {
    ZSTD_d_windowLogMax = 100,
    ZSTD_d_experimentalParam1 = 1000,
    ZSTD_d_experimentalParam2 = 1001,
    ZSTD_d_experimentalParam3 = 1002,
    ZSTD_d_experimentalParam4 = 1003,
}
extern "C" {
    #[doc = " ZSTD_dParam_getBounds() :"]
    #[doc = "  All parameters must belong to an interval with lower and upper bounds,"]
    #[doc = "  otherwise they will either trigger an error or be automatically clamped."]
    #[doc = " @return : a structure, ZSTD_bounds, which contains"]
    #[doc = "         - an error status field, which must be tested using ZSTD_isError()"]
    #[doc = "         - both lower and upper bounds, inclusive"]
    pub fn ZSTD_dParam_getBounds(dParam: ZSTD_dParameter) -> ZSTD_bounds;
}
extern "C" {
    #[doc = " ZSTD_DCtx_setParameter() :"]
    #[doc = "  Set one compression parameter, selected by enum ZSTD_dParameter."]
    #[doc = "  All parameters have valid bounds. Bounds can be queried using ZSTD_dParam_getBounds()."]
    #[doc = "  Providing a value beyond bound will either clamp it, or trigger an error (depending on parameter)."]
    #[doc = "  Setting a parameter is only possible during frame initialization (before starting decompression)."]
    #[doc = " @return : 0, or an error code (which can be tested using ZSTD_isError())."]
    pub fn ZSTD_DCtx_setParameter(
        dctx: *mut ZSTD_DCtx,
        param: ZSTD_dParameter,
        value: ::std::os::raw::c_int,
    ) -> usize;
}
extern "C" {
    #[doc = " ZSTD_DCtx_reset() :"]
    #[doc = "  Return a DCtx to clean state."]
    #[doc = "  Session and parameters can be reset jointly or separately."]
    #[doc = "  Parameters can only be reset when no active frame is being decompressed."]
    #[doc = " @return : 0, or an error code, which can be tested with ZSTD_isError()"]
    pub fn ZSTD_DCtx_reset(dctx: *mut ZSTD_DCtx, reset: ZSTD_ResetDirective) -> usize;
}
#[doc = "  Streaming"]
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ZSTD_inBuffer_s {
    #[doc = "< start of input buffer"]
    pub src: *const ::core::ffi::c_void,
    #[doc = "< size of input buffer"]
    pub size: usize,
    #[doc = "< position where reading stopped. Will be updated. Necessarily 0 <= pos <= size"]
    pub pos: usize,
}
#[doc = "  Streaming"]
pub type ZSTD_inBuffer = ZSTD_inBuffer_s;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ZSTD_outBuffer_s {
    #[doc = "< start of output buffer"]
    pub dst: *mut ::core::ffi::c_void,
    #[doc = "< size of output buffer"]
    pub size: usize,
    #[doc = "< position where writing stopped. Will be updated. Necessarily 0 <= pos <= size"]
    pub pos: usize,
}
pub type ZSTD_outBuffer = ZSTD_outBuffer_s;
pub type ZSTD_CStream = ZSTD_CCtx;
extern "C" {
    pub fn ZSTD_createCStream() -> *mut ZSTD_CStream;
}
extern "C" {
    pub fn ZSTD_freeCStream(zcs: *mut ZSTD_CStream) -> usize;
}
#[repr(u32)]
#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub enum ZSTD_EndDirective {
    ZSTD_e_continue = 0,
    ZSTD_e_flush = 1,
    ZSTD_e_end = 2,
}
extern "C" {
    #[doc = " ZSTD_compressStream2() : Requires v1.4.0+"]
    #[doc = "  Behaves about the same as ZSTD_compressStream, with additional control on end directive."]
    #[doc = "  - Compression parameters are pushed into CCtx before starting compression, using ZSTD_CCtx_set*()"]
    #[doc = "  - Compression parameters cannot be changed once compression is started (save a list of exceptions in multi-threading mode)"]
    #[doc = "  - output->pos must be <= dstCapacity, input->pos must be <= srcSize"]
    #[doc = "  - output->pos and input->pos will be updated. They are guaranteed to remain below their respective limit."]
    #[doc = "  - endOp must be a valid directive"]
    #[doc = "  - When nbWorkers==0 (default), function is blocking : it completes its job before returning to caller."]
    #[doc = "  - When nbWorkers>=1, function is non-blocking : it copies a portion of input, distributes jobs to internal worker threads, flush to output whatever is available,"]
    #[doc = "                                                  and then immediately returns, just indicating that there is some data remaining to be flushed."]
    #[doc = "                                                  The function nonetheless guarantees forward progress : it will return only after it reads or write at least 1+ byte."]
    #[doc = "  - Exception : if the first call requests a ZSTD_e_end directive and provides enough dstCapacity, the function delegates to ZSTD_compress2() which is always blocking."]
    #[doc = "  - @return provides a minimum amount of data remaining to be flushed from internal buffers"]
    #[doc = "            or an error code, which can be tested using ZSTD_isError()."]
    #[doc = "            if @return != 0, flush is not fully completed, there is still some data left within internal buffers."]
    #[doc = "            This is useful for ZSTD_e_flush, since in this case more flushes are necessary to empty all buffers."]
    #[doc = "            For ZSTD_e_end, @return == 0 when internal buffers are fully flushed and frame is completed."]
    #[doc = "  - after a ZSTD_e_end directive, if internal buffer is not fully flushed (@return != 0),"]
    #[doc = "            only ZSTD_e_end or ZSTD_e_flush operations are allowed."]
    #[doc = "            Before starting a new compression job, or changing compression parameters,"]
    #[doc = "            it is required to fully flush internal buffers."]
    pub fn ZSTD_compressStream2(
        cctx: *mut ZSTD_CCtx,
        output: *mut ZSTD_outBuffer,
        input: *mut ZSTD_inBuffer,
        endOp: ZSTD_EndDirective,
    ) -> usize;
}
extern "C" {
    pub fn ZSTD_CStreamInSize() -> usize;
}
extern "C" {
    pub fn ZSTD_CStreamOutSize() -> usize;
}
extern "C" {
    #[doc = " Equivalent to:"]
    #[doc = ""]
    #[doc = "     ZSTD_CCtx_reset(zcs, ZSTD_reset_session_only);"]
    #[doc = "     ZSTD_CCtx_refCDict(zcs, NULL); // clear the dictionary (if any)"]
    #[doc = "     ZSTD_CCtx_setParameter(zcs, ZSTD_c_compressionLevel, compressionLevel);"]
    pub fn ZSTD_initCStream(
        zcs: *mut ZSTD_CStream,
        compressionLevel: ::std::os::raw::c_int,
    ) -> usize;
}
extern "C" {
    #[doc = " Alternative for ZSTD_compressStream2(zcs, output, input, ZSTD_e_continue)."]
    #[doc = " NOTE: The return value is different. ZSTD_compressStream() returns a hint for"]
    #[doc = " the next read size (if non-zero and not an error). ZSTD_compressStream2()"]
    #[doc = " returns the minimum nb of bytes left to flush (if non-zero and not an error)."]
    pub fn ZSTD_compressStream(
        zcs: *mut ZSTD_CStream,
        output: *mut ZSTD_outBuffer,
        input: *mut ZSTD_inBuffer,
    ) -> usize;
}
extern "C" {
    #[doc = " Equivalent to ZSTD_compressStream2(zcs, output, &emptyInput, ZSTD_e_flush)."]
    pub fn ZSTD_flushStream(zcs: *mut ZSTD_CStream, output: *mut ZSTD_outBuffer) -> usize;
}
extern "C" {
    #[doc = " Equivalent to ZSTD_compressStream2(zcs, output, &emptyInput, ZSTD_e_end)."]
    pub fn ZSTD_endStream(zcs: *mut ZSTD_CStream, output: *mut ZSTD_outBuffer) -> usize;
}
pub type ZSTD_DStream = ZSTD_DCtx;
extern "C" {
    pub fn ZSTD_createDStream() -> *mut ZSTD_DStream;
}
extern "C" {
    pub fn ZSTD_freeDStream(zds: *mut ZSTD_DStream) -> usize;
}
extern "C" {
    pub fn ZSTD_initDStream(zds: *mut ZSTD_DStream) -> usize;
}
extern "C" {
    pub fn ZSTD_decompressStream(
        zds: *mut ZSTD_DStream,
        output: *mut ZSTD_outBuffer,
        input: *mut ZSTD_inBuffer,
    ) -> usize;
}
extern "C" {
    pub fn ZSTD_DStreamInSize() -> usize;
}
extern "C" {
    pub fn ZSTD_DStreamOutSize() -> usize;
}
extern "C" {
    #[doc = "  Simple dictionary API"]
    #[doc = "  Compression at an explicit compression level using a Dictionary."]
    #[doc = "  A dictionary can be any arbitrary data segment (also called a prefix),"]
    #[doc = "  or a buffer with specified information (see zdict.h)."]
    #[doc = "  Note : This function loads the dictionary, resulting in significant startup delay."]
    #[doc = "         It's intended for a dictionary used only once."]
    #[doc = "  Note 2 : When `dict == NULL || dictSize < 8` no dictionary is used."]
    pub fn ZSTD_compress_usingDict(
        ctx: *mut ZSTD_CCtx,
        dst: *mut ::core::ffi::c_void,
        dstCapacity: usize,
        src: *const ::core::ffi::c_void,
        srcSize: usize,
        dict: *const ::core::ffi::c_void,
        dictSize: usize,
        compressionLevel: ::std::os::raw::c_int,
    ) -> usize;
}
extern "C" {
    #[doc = " ZSTD_decompress_usingDict() :"]
    #[doc = "  Decompression using a known Dictionary."]
    #[doc = "  Dictionary must be identical to the one used during compression."]
    #[doc = "  Note : This function loads the dictionary, resulting in significant startup delay."]
    #[doc = "         It's intended for a dictionary used only once."]
    #[doc = "  Note : When `dict == NULL || dictSize < 8` no dictionary is used."]
    pub fn ZSTD_decompress_usingDict(
        dctx: *mut ZSTD_DCtx,
        dst: *mut ::core::ffi::c_void,
        dstCapacity: usize,
        src: *const ::core::ffi::c_void,
        srcSize: usize,
        dict: *const ::core::ffi::c_void,
        dictSize: usize,
    ) -> usize;
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ZSTD_CDict_s {
    _unused: [u8; 0],
}
#[doc = "  Bulk processing dictionary API"]
pub type ZSTD_CDict = ZSTD_CDict_s;
extern "C" {
    #[doc = " ZSTD_createCDict() :"]
    #[doc = "  When compressing multiple messages or blocks using the same dictionary,"]
    #[doc = "  it's recommended to digest the dictionary only once, since it's a costly operation."]
    #[doc = "  ZSTD_createCDict() will create a state from digesting a dictionary."]
    #[doc = "  The resulting state can be used for future compression operations with very limited startup cost."]
    #[doc = "  ZSTD_CDict can be created once and shared by multiple threads concurrently, since its usage is read-only."]
    #[doc = " @dictBuffer can be released after ZSTD_CDict creation, because its content is copied within CDict."]
    #[doc = "  Note 1 : Consider experimental function `ZSTD_createCDict_byReference()` if you prefer to not duplicate @dictBuffer content."]
    #[doc = "  Note 2 : A ZSTD_CDict can be created from an empty @dictBuffer,"]
    #[doc = "      in which case the only thing that it transports is the @compressionLevel."]
    #[doc = "      This can be useful in a pipeline featuring ZSTD_compress_usingCDict() exclusively,"]
    #[doc = "      expecting a ZSTD_CDict parameter with any data, including those without a known dictionary."]
    pub fn ZSTD_createCDict(
        dictBuffer: *const ::core::ffi::c_void,
        dictSize: usize,
        compressionLevel: ::std::os::raw::c_int,
    ) -> *mut ZSTD_CDict;
}
extern "C" {
    #[doc = " ZSTD_freeCDict() :"]
    #[doc = "  Function frees memory allocated by ZSTD_createCDict()."]
    #[doc = "  If a NULL pointer is passed, no operation is performed."]
    pub fn ZSTD_freeCDict(CDict: *mut ZSTD_CDict) -> usize;
}
extern "C" {
    #[doc = " ZSTD_compress_usingCDict() :"]
    #[doc = "  Compression using a digested Dictionary."]
    #[doc = "  Recommended when same dictionary is used multiple times."]
    #[doc = "  Note : compression level is _decided at dictionary creation time_,"]
    #[doc = "     and frame parameters are hardcoded (dictID=yes, contentSize=yes, checksum=no)"]
    pub fn ZSTD_compress_usingCDict(
        cctx: *mut ZSTD_CCtx,
        dst: *mut ::core::ffi::c_void,
        dstCapacity: usize,
        src: *const ::core::ffi::c_void,
        srcSize: usize,
        cdict: *const ZSTD_CDict,
    ) -> usize;
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ZSTD_DDict_s {
    _unused: [u8; 0],
}
pub type ZSTD_DDict = ZSTD_DDict_s;
extern "C" {
    #[doc = " ZSTD_createDDict() :"]
    #[doc = "  Create a digested dictionary, ready to start decompression operation without startup delay."]
    #[doc = "  dictBuffer can be released after DDict creation, as its content is copied inside DDict."]
    pub fn ZSTD_createDDict(
        dictBuffer: *const ::core::ffi::c_void,
        dictSize: usize,
    ) -> *mut ZSTD_DDict;
}
extern "C" {
    #[doc = " ZSTD_freeDDict() :"]
    #[doc = "  Function frees memory allocated with ZSTD_createDDict()"]
    #[doc = "  If a NULL pointer is passed, no operation is performed."]
    pub fn ZSTD_freeDDict(ddict: *mut ZSTD_DDict) -> usize;
}
extern "C" {
    #[doc = " ZSTD_decompress_usingDDict() :"]
    #[doc = "  Decompression using a digested Dictionary."]
    #[doc = "  Recommended when same dictionary is used multiple times."]
    pub fn ZSTD_decompress_usingDDict(
        dctx: *mut ZSTD_DCtx,
        dst: *mut ::core::ffi::c_void,
        dstCapacity: usize,
        src: *const ::core::ffi::c_void,
        srcSize: usize,
        ddict: *const ZSTD_DDict,
    ) -> usize;
}
extern "C" {
    #[doc = " ZSTD_getDictID_fromDict() : Requires v1.4.0+"]
    #[doc = "  Provides the dictID stored within dictionary."]
    #[doc = "  if @return == 0, the dictionary is not conformant with Zstandard specification."]
    #[doc = "  It can still be loaded, but as a content-only dictionary."]
    pub fn ZSTD_getDictID_fromDict(
        dict: *const ::core::ffi::c_void,
        dictSize: usize,
    ) -> ::std::os::raw::c_uint;
}
extern "C" {
    #[doc = " ZSTD_getDictID_fromCDict() : Requires v1.5.0+"]
    #[doc = "  Provides the dictID of the dictionary loaded into `cdict`."]
    #[doc = "  If @return == 0, the dictionary is not conformant to Zstandard specification, or empty."]
    #[doc = "  Non-conformant dictionaries can still be loaded, but as content-only dictionaries."]
    pub fn ZSTD_getDictID_fromCDict(cdict: *const ZSTD_CDict) -> ::std::os::raw::c_uint;
}
extern "C" {
    #[doc = " ZSTD_getDictID_fromDDict() : Requires v1.4.0+"]
    #[doc = "  Provides the dictID of the dictionary loaded into `ddict`."]
    #[doc = "  If @return == 0, the dictionary is not conformant to Zstandard specification, or empty."]
    #[doc = "  Non-conformant dictionaries can still be loaded, but as content-only dictionaries."]
    pub fn ZSTD_getDictID_fromDDict(ddict: *const ZSTD_DDict) -> ::std::os::raw::c_uint;
}
extern "C" {
    #[doc = " ZSTD_getDictID_fromFrame() : Requires v1.4.0+"]
    #[doc = "  Provides the dictID required to decompressed the frame stored within `src`."]
    #[doc = "  If @return == 0, the dictID could not be decoded."]
    #[doc = "  This could for one of the following reasons :"]
    #[doc = "  - The frame does not require a dictionary to be decoded (most common case)."]
    #[doc = "  - The frame was built with dictID intentionally removed. Whatever dictionary is necessary is a hidden information."]
    #[doc = "    Note : this use case also happens when using a non-conformant dictionary."]
    #[doc = "  - `srcSize` is too small, and as a result, the frame header could not be decoded (only possible if `srcSize < ZSTD_FRAMEHEADERSIZE_MAX`)."]
    #[doc = "  - This is not a Zstandard frame."]
    #[doc = "  When identifying the exact failure cause, it's possible to use ZSTD_getFrameHeader(), which will provide a more precise error code."]
    pub fn ZSTD_getDictID_fromFrame(
        src: *const ::core::ffi::c_void,
        srcSize: usize,
    ) -> ::std::os::raw::c_uint;
}
extern "C" {
    #[doc = " ZSTD_CCtx_loadDictionary() : Requires v1.4.0+"]
    #[doc = "  Create an internal CDict from `dict` buffer."]
    #[doc = "  Decompression will have to use same dictionary."]
    #[doc = " @result : 0, or an error code (which can be tested with ZSTD_isError())."]
    #[doc = "  Special: Loading a NULL (or 0-size) dictionary invalidates previous dictionary,"]
    #[doc = "           meaning \"return to no-dictionary mode\"."]
    #[doc = "  Note 1 : Dictionary is sticky, it will be used for all future compressed frames."]
    #[doc = "           To return to \"no-dictionary\" situation, load a NULL dictionary (or reset parameters)."]
    #[doc = "  Note 2 : Loading a dictionary involves building tables."]
    #[doc = "           It's also a CPU consuming operation, with non-negligible impact on latency."]
    #[doc = "           Tables are dependent on compression parameters, and for this reason,"]
    #[doc = "           compression parameters can no longer be changed after loading a dictionary."]
    #[doc = "  Note 3 :`dict` content will be copied internally."]
    #[doc = "           Use experimental ZSTD_CCtx_loadDictionary_byReference() to reference content instead."]
    #[doc = "           In such a case, dictionary buffer must outlive its users."]
    #[doc = "  Note 4 : Use ZSTD_CCtx_loadDictionary_advanced()"]
    #[doc = "           to precisely select how dictionary content must be interpreted."]
    pub fn ZSTD_CCtx_loadDictionary(
        cctx: *mut ZSTD_CCtx,
        dict: *const ::core::ffi::c_void,
        dictSize: usize,
    ) -> usize;
}
extern "C" {
    #[doc = " ZSTD_CCtx_refCDict() : Requires v1.4.0+"]
    #[doc = "  Reference a prepared dictionary, to be used for all next compressed frames."]
    #[doc = "  Note that compression parameters are enforced from within CDict,"]
    #[doc = "  and supersede any compression parameter previously set within CCtx."]
    #[doc = "  The parameters ignored are labelled as \"superseded-by-cdict\" in the ZSTD_cParameter enum docs."]
    #[doc = "  The ignored parameters will be used again if the CCtx is returned to no-dictionary mode."]
    #[doc = "  The dictionary will remain valid for future compressed frames using same CCtx."]
    #[doc = " @result : 0, or an error code (which can be tested with ZSTD_isError())."]
    #[doc = "  Special : Referencing a NULL CDict means \"return to no-dictionary mode\"."]
    #[doc = "  Note 1 : Currently, only one dictionary can be managed."]
    #[doc = "           Referencing a new dictionary effectively \"discards\" any previous one."]
    #[doc = "  Note 2 : CDict is just referenced, its lifetime must outlive its usage within CCtx."]
    pub fn ZSTD_CCtx_refCDict(cctx: *mut ZSTD_CCtx, cdict: *const ZSTD_CDict) -> usize;
}
extern "C" {
    #[doc = " ZSTD_CCtx_refPrefix() : Requires v1.4.0+"]
    #[doc = "  Reference a prefix (single-usage dictionary) for next compressed frame."]
    #[doc = "  A prefix is **only used once**. Tables are discarded at end of frame (ZSTD_e_end)."]
    #[doc = "  Decompression will need same prefix to properly regenerate data."]
    #[doc = "  Compressing with a prefix is similar in outcome as performing a diff and compressing it,"]
    #[doc = "  but performs much faster, especially during decompression (compression speed is tunable with compression level)."]
    #[doc = " @result : 0, or an error code (which can be tested with ZSTD_isError())."]
    #[doc = "  Special: Adding any prefix (including NULL) invalidates any previous prefix or dictionary"]
    #[doc = "  Note 1 : Prefix buffer is referenced. It **must** outlive compression."]
    #[doc = "           Its content must remain unmodified during compression."]
    #[doc = "  Note 2 : If the intention is to diff some large src data blob with some prior version of itself,"]
    #[doc = "           ensure that the window size is large enough to contain the entire source."]
    #[doc = "           See ZSTD_c_windowLog."]
    #[doc = "  Note 3 : Referencing a prefix involves building tables, which are dependent on compression parameters."]
    #[doc = "           It's a CPU consuming operation, with non-negligible impact on latency."]
    #[doc = "           If there is a need to use the same prefix multiple times, consider loadDictionary instead."]
    #[doc = "  Note 4 : By default, the prefix is interpreted as raw content (ZSTD_dct_rawContent)."]
    #[doc = "           Use experimental ZSTD_CCtx_refPrefix_advanced() to alter dictionary interpretation."]
    pub fn ZSTD_CCtx_refPrefix(
        cctx: *mut ZSTD_CCtx,
        prefix: *const ::core::ffi::c_void,
        prefixSize: usize,
    ) -> usize;
}
extern "C" {
    #[doc = " ZSTD_DCtx_loadDictionary() : Requires v1.4.0+"]
    #[doc = "  Create an internal DDict from dict buffer,"]
    #[doc = "  to be used to decompress next frames."]
    #[doc = "  The dictionary remains valid for all future frames, until explicitly invalidated."]
    #[doc = " @result : 0, or an error code (which can be tested with ZSTD_isError())."]
    #[doc = "  Special : Adding a NULL (or 0-size) dictionary invalidates any previous dictionary,"]
    #[doc = "            meaning \"return to no-dictionary mode\"."]
    #[doc = "  Note 1 : Loading a dictionary involves building tables,"]
    #[doc = "           which has a non-negligible impact on CPU usage and latency."]
    #[doc = "           It's recommended to \"load once, use many times\", to amortize the cost"]
    #[doc = "  Note 2 :`dict` content will be copied internally, so `dict` can be released after loading."]
    #[doc = "           Use ZSTD_DCtx_loadDictionary_byReference() to reference dictionary content instead."]
    #[doc = "  Note 3 : Use ZSTD_DCtx_loadDictionary_advanced() to take control of"]
    #[doc = "           how dictionary content is loaded and interpreted."]
    pub fn ZSTD_DCtx_loadDictionary(
        dctx: *mut ZSTD_DCtx,
        dict: *const ::core::ffi::c_void,
        dictSize: usize,
    ) -> usize;
}
extern "C" {
    #[doc = " ZSTD_DCtx_refDDict() : Requires v1.4.0+"]
    #[doc = "  Reference a prepared dictionary, to be used to decompress next frames."]
    #[doc = "  The dictionary remains active for decompression of future frames using same DCtx."]
    #[doc = ""]
    #[doc = "  If called with ZSTD_d_refMultipleDDicts enabled, repeated calls of this function"]
    #[doc = "  will store the DDict references in a table, and the DDict used for decompression"]
    #[doc = "  will be determined at decompression time, as per the dict ID in the frame."]
    #[doc = "  The memory for the table is allocated on the first call to refDDict, and can be"]
    #[doc = "  freed with ZSTD_freeDCtx()."]
    #[doc = ""]
    #[doc = " @result : 0, or an error code (which can be tested with ZSTD_isError())."]
    #[doc = "  Note 1 : Currently, only one dictionary can be managed."]
    #[doc = "           Referencing a new dictionary effectively \"discards\" any previous one."]
    #[doc = "  Special: referencing a NULL DDict means \"return to no-dictionary mode\"."]
    #[doc = "  Note 2 : DDict is just referenced, its lifetime must outlive its usage from DCtx."]
    pub fn ZSTD_DCtx_refDDict(dctx: *mut ZSTD_DCtx, ddict: *const ZSTD_DDict) -> usize;
}
extern "C" {
    #[doc = " ZSTD_DCtx_refPrefix() : Requires v1.4.0+"]
    #[doc = "  Reference a prefix (single-usage dictionary) to decompress next frame."]
    #[doc = "  This is the reverse operation of ZSTD_CCtx_refPrefix(),"]
    #[doc = "  and must use the same prefix as the one used during compression."]
    #[doc = "  Prefix is **only used once**. Reference is discarded at end of frame."]
    #[doc = "  End of frame is reached when ZSTD_decompressStream() returns 0."]
    #[doc = " @result : 0, or an error code (which can be tested with ZSTD_isError())."]
    #[doc = "  Note 1 : Adding any prefix (including NULL) invalidates any previously set prefix or dictionary"]
    #[doc = "  Note 2 : Prefix buffer is referenced. It **must** outlive decompression."]
    #[doc = "           Prefix buffer must remain unmodified up to the end of frame,"]
    #[doc = "           reached when ZSTD_decompressStream() returns 0."]
    #[doc = "  Note 3 : By default, the prefix is treated as raw content (ZSTD_dct_rawContent)."]
    #[doc = "           Use ZSTD_CCtx_refPrefix_advanced() to alter dictMode (Experimental section)"]
    #[doc = "  Note 4 : Referencing a raw content prefix has almost no cpu nor memory cost."]
    #[doc = "           A full dictionary is more costly, as it requires building tables."]
    pub fn ZSTD_DCtx_refPrefix(
        dctx: *mut ZSTD_DCtx,
        prefix: *const ::core::ffi::c_void,
        prefixSize: usize,
    ) -> usize;
}
extern "C" {
    #[doc = " ZSTD_sizeof_*() : Requires v1.4.0+"]
    #[doc = "  These functions give the _current_ memory usage of selected object."]
    #[doc = "  Note that object memory usage can evolve (increase or decrease) over time."]
    pub fn ZSTD_sizeof_CCtx(cctx: *const ZSTD_CCtx) -> usize;
}
extern "C" {
    pub fn ZSTD_sizeof_DCtx(dctx: *const ZSTD_DCtx) -> usize;
}
extern "C" {
    pub fn ZSTD_sizeof_CStream(zcs: *const ZSTD_CStream) -> usize;
}
extern "C" {
    pub fn ZSTD_sizeof_DStream(zds: *const ZSTD_DStream) -> usize;
}
extern "C" {
    pub fn ZSTD_sizeof_CDict(cdict: *const ZSTD_CDict) -> usize;
}
extern "C" {
    pub fn ZSTD_sizeof_DDict(ddict: *const ZSTD_DDict) -> usize;
}
