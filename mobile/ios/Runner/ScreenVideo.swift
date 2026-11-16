import CoreMedia
import CoreVideo
import Flutter
import VideoToolbox

/// 기기 화면 거울의 영상 방식(`docs/screen-stream.md` S2) — 데스크탑이 VideoToolbox 로 압축해 보낸 장을 이 폰의 VideoToolbox 로
/// 풀어 Flutter 텍스처로 내준다. 장은 `'F'` 틀(`kasa_remote::screen_stream::Frame`)째로 받는다.
///
/// 채널: `kasaterm/screen_video` 메서드 — `create` → 텍스처 번호, `dispose`. 네이티브 → 다트 `keyframe`(키프레임이 필요하다).
/// `kasaterm/screen_video/frames` 바이너리 — 텍스처 번호(i64 LE) + 장 하나, 또는 `R` 한 바이트(새 연결, 키프레임부터 다시). 리셋을 장과
/// 같은 채널에 싣는 까닭: 채널이 다르면 차례가 엇갈려 첫 키프레임을 버린 채 기다릴 수 있다.
final class ScreenVideo: NSObject, FlutterTexture {
  private let registry: FlutterTextureRegistry
  private let needKeyframe: () -> Void
  private let queue = DispatchQueue(label: "kasaterm.screen-video.decode")
  private let lock = NSLock()
  private var latest: CVPixelBuffer?
  private var session: VTDecompressionSession?
  private var format: CMVideoFormatDescription?
  private var paramSets: [Data] = []
  private var waitingKey = true
  private var askedAt = Date.distantPast
  var textureId: Int64 = 0

  init(registry: FlutterTextureRegistry, needKeyframe: @escaping () -> Void) {
    self.registry = registry
    self.needKeyframe = needKeyframe
  }

  func copyPixelBuffer() -> Unmanaged<CVPixelBuffer>? {
    lock.lock()
    defer { lock.unlock() }
    guard let pb = latest else { return nil }
    return Unmanaged.passRetained(pb)
  }

  func push(_ frame: Data) {
    queue.async { [weak self] in
      if frame.count == 1, frame.first == 0x52 {
        self?.waitingKey = true
      } else {
        self?.decode(frame)
      }
    }
  }

  func close() {
    queue.sync {
      if let s = session { VTDecompressionSessionInvalidate(s) }
      session = nil
    }
    registry.unregisterTexture(textureId)
  }

  private struct Head {
    var key: Bool
    var paramSets: [Data]
    var payload: Data
  }

  /// `'F'`·seq u32·cap_id u64·key u8·t_cap f64·t_enc f64·가로 u32·세로 u32·파라미터 셋 수 u8·(u16 길이 + 바이트)… + NAL.
  private static func parse(_ d: Data) -> Head? {
    let b = [UInt8](d)
    var at = 0
    func take(_ n: Int) -> ArraySlice<UInt8>? {
      guard at + n <= b.count else { return nil }
      defer { at += n }
      return b[at..<at + n]
    }
    guard let tag = take(1), tag.first == 0x46, take(12) != nil, let key = take(1), take(24) != nil,
          let count = take(1)?.first else { return nil }
    var sets: [Data] = []
    for _ in 0..<Int(count) {
      guard let len = take(2) else { return nil }
      let n = Int(len[len.startIndex]) << 8 | Int(len[len.startIndex + 1])
      guard let ps = take(n) else { return nil }
      sets.append(Data(ps))
    }
    return Head(key: key.first != 0, paramSets: sets, payload: Data(b[at...]))
  }

  private func decode(_ frame: Data) {
    guard let head = Self.parse(frame) else { return }
    if head.key, !head.paramSets.isEmpty, head.paramSets != paramSets {
      guard rebuild(head.paramSets) else { return fail() }
    }
    if waitingKey {
      guard head.key, session != nil else { return askKeyframe() }
      waitingKey = false
    }
    guard let session, let format, let sample = Self.sample(head.payload, format: format) else { return fail() }
    let status = VTDecompressionSessionDecodeFrame(session, sampleBuffer: sample, flags: [], infoFlagsOut: nil) {
      [weak self] status, _, image, _, _ in
      guard let self else { return }
      guard status == noErr, let image else { return self.fail() }
      self.lock.lock()
      self.latest = image
      self.lock.unlock()
      self.registry.textureFrameAvailable(self.textureId)
    }
    if status != noErr { fail() }
  }

  /// 장마다 앞 장을 참조하므로 하나라도 못 풀면 다음 키프레임까지 버리고 키프레임을 청한다.
  private func fail() {
    waitingKey = true
    askKeyframe()
  }

  /// 키프레임을 기다리는 동안 일반 장이 오면 다시 청한다 — 0.5초에 한 번.
  private func askKeyframe() {
    guard Date().timeIntervalSince(askedAt) > 0.5 else { return }
    askedAt = Date()
    DispatchQueue.main.async { self.needKeyframe() }
  }

  private func rebuild(_ sets: [Data]) -> Bool {
    if let s = session { VTDecompressionSessionInvalidate(s) }
    session = nil
    format = nil
    var desc: CMFormatDescription?
    let status: OSStatus = sets.withPointers { ptrs, sizes in
      // 파라미터 셋이 셋(VPS·SPS·PPS)이면 HEVC, 둘(SPS·PPS)이면 H.264.
      if sets.count >= 3 {
        return CMVideoFormatDescriptionCreateFromHEVCParameterSets(
          allocator: kCFAllocatorDefault, parameterSetCount: sets.count, parameterSetPointers: ptrs,
          parameterSetSizes: sizes, nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &desc)
      }
      return CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: kCFAllocatorDefault, parameterSetCount: sets.count, parameterSetPointers: ptrs,
        parameterSetSizes: sizes, nalUnitHeaderLength: 4, formatDescriptionOut: &desc)
    }
    guard status == noErr, let desc else { return false }
    let attrs: [CFString: Any] = [
      kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
      kCVPixelBufferIOSurfacePropertiesKey: [:] as [CFString: Any],
      kCVPixelBufferMetalCompatibilityKey: true,
    ]
    var out: VTDecompressionSession?
    let made = VTDecompressionSessionCreate(
      allocator: kCFAllocatorDefault, formatDescription: desc, decoderSpecification: nil,
      imageBufferAttributes: attrs as CFDictionary, outputCallback: nil, decompressionSessionOut: &out)
    guard made == noErr, let out else { return false }
    VTSessionSetProperty(out, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
    session = out
    format = desc
    paramSets = sets
    return true
  }

  private static func sample(_ payload: Data, format: CMVideoFormatDescription) -> CMSampleBuffer? {
    var block: CMBlockBuffer?
    let len = payload.count
    guard CMBlockBufferCreateWithMemoryBlock(
      allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: len, blockAllocator: kCFAllocatorDefault,
      customBlockSource: nil, offsetToData: 0, dataLength: len, flags: 0, blockBufferOut: &block) == noErr,
      let block else { return nil }
    let copied = payload.withUnsafeBytes { raw in
      CMBlockBufferReplaceDataBytes(with: raw.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: len)
    }
    guard copied == noErr else { return nil }
    var sample: CMSampleBuffer?
    var size = len
    guard CMSampleBufferCreateReady(
      allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format, sampleCount: 1,
      sampleTimingEntryCount: 0, sampleTimingArray: nil, sampleSizeEntryCount: 1, sampleSizeArray: &size,
      sampleBufferOut: &sample) == noErr else { return nil }
    return sample
  }
}

/// 다트와 잇는 채널. 화면 거울 화면 하나에 텍스처 하나.
final class ScreenVideoPlugin {
  private let registry: FlutterTextureRegistry
  private let channel: FlutterMethodChannel
  private let frames: FlutterBasicMessageChannel
  private var videos: [Int64: ScreenVideo] = [:]

  init(registrar: FlutterPluginRegistrar) {
    registry = registrar.textures()
    channel = FlutterMethodChannel(name: "kasaterm/screen_video", binaryMessenger: registrar.messenger())
    frames = FlutterBasicMessageChannel(
      name: "kasaterm/screen_video/frames", binaryMessenger: registrar.messenger(), codec: FlutterBinaryCodec.sharedInstance())
    channel.setMethodCallHandler { [weak self] call, result in self?.handle(call, result) }
    // 장 앞 8바이트는 텍스처 번호(i64 LE) — 장마다 채널을 따로 두지 않는다.
    frames.setMessageHandler { [weak self] message, reply in
      if let data = message as? Data, data.count > 8 {
        let id = data.prefix(8).withUnsafeBytes { $0.loadUnaligned(as: Int64.self) }
        self?.videos[id]?.push(data.dropFirst(8))
      }
      reply(nil)
    }
  }

  private func handle(_ call: FlutterMethodCall, _ result: FlutterResult) {
    let id = (call.arguments as? [String: Any])?["id"] as? Int64
    switch call.method {
    case "create":
      var made: Int64 = 0
      let video = ScreenVideo(registry: registry) { [weak self] in
        self?.channel.invokeMethod("keyframe", arguments: ["id": made])
      }
      made = registry.register(video)
      video.textureId = made
      videos[made] = video
      result(made)
    case "dispose":
      if let id { videos.removeValue(forKey: id)?.close() }
      result(nil)
    default:
      result(FlutterMethodNotImplemented)
    }
  }
}

private extension Array where Element == Data {
  /// 파라미터 셋 바이트들의 포인터·길이 배열을 잠시 빌려준다(CoreMedia 가 그 동안만 읽는다).
  func withPointers<R>(_ body: (UnsafePointer<UnsafePointer<UInt8>>, UnsafePointer<Int>) -> R) -> R {
    let bufs = map { d -> UnsafeMutablePointer<UInt8> in
      let p = UnsafeMutablePointer<UInt8>.allocate(capacity: Swift.max(d.count, 1))
      d.copyBytes(to: p, count: d.count)
      return p
    }
    defer { bufs.forEach { $0.deallocate() } }
    let ptrs = bufs.map { UnsafePointer($0) }
    let sizes = map(\.count)
    return ptrs.withUnsafeBufferPointer { p in sizes.withUnsafeBufferPointer { s in body(p.baseAddress!, s.baseAddress!) } }
  }
}
