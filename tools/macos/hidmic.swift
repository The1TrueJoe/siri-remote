// Open every HID device macOS created for the Siri Remote and print its input
// reports. The point is the virtual Microphone device (Consumer page, usage
// 0x04): if opening it makes Apple's driver start the audio stream, the
// Bluetooth log will show the keyhole command it sends to the remote.
import Foundation
import IOKit.hid

setvbuf(stdout, nil, _IOLBF, 0)
let mgr = IOHIDManagerCreate(kCFAllocatorDefault, IOOptionBits(kIOHIDOptionsTypeNone))
IOHIDManagerSetDeviceMatching(mgr, [kIOHIDVendorIDKey: 76, kIOHIDProductIDKey: 614] as CFDictionary)
IOHIDManagerScheduleWithRunLoop(mgr, CFRunLoopGetCurrent(), CFRunLoopMode.defaultMode.rawValue)
let rc = IOHIDManagerOpen(mgr, IOOptionBits(kIOHIDOptionsTypeNone))
let rcBits = UInt32(bitPattern: rc)
print(String(format: "IOHIDManagerOpen -> 0x%08x%@", rcBits,
             rcBits == 0 ? " (ok)" : rcBits == 0xe00002e2 ? " (not permitted: needs Input Monitoring)" : ""))

guard let devs = IOHIDManagerCopyDevices(mgr) as? Set<IOHIDDevice>, !devs.isEmpty else {
    print("no Siri Remote HID devices found"); exit(1)
}

func prop(_ d: IOHIDDevice, _ k: String) -> Int {
    (IOHIDDeviceGetProperty(d, k as CFString) as? NSNumber)?.intValue ?? -1
}

var counts: [Int: Int] = [:]
for d in devs {
    let page = prop(d, kIOHIDPrimaryUsagePageKey), usage = prop(d, kIOHIDPrimaryUsageKey)
    let maxIn = prop(d, kIOHIDMaxInputReportSizeKey)
    let tag = (page << 16) | usage
    print(String(format: "device page 0x%04x usage 0x%04x  maxInput %d%@", page, usage, maxIn,
                 page == 0x0c && usage == 0x04 ? "   <-- MICROPHONE" : ""))
    let size = max(maxIn, 512)
    let buf = UnsafeMutablePointer<UInt8>.allocate(capacity: size)
    IOHIDDeviceRegisterInputReportCallback(d, buf, size, { ctx, _, _, _, reportID, report, length in
        let tag = Int(bitPattern: ctx)
        let page = tag >> 16, usage = tag & 0xffff
        let n = min(length, 32)
        let hex = (0..<n).map { String(format: "%02x", report[$0]) }.joined(separator: " ")
        let ts = String(format: "%.3f", Date().timeIntervalSince1970.truncatingRemainder(dividingBy: 1000))
        print("[\(ts)] page 0x\(String(page, radix: 16)) usage 0x\(String(usage, radix: 16)) id=\(reportID) len=\(length)  \(hex)")
    }, UnsafeMutableRawPointer(bitPattern: tag))
}
print("\nlistening -- hold the microphone button and talk\n")
CFRunLoopRunInMode(CFRunLoopMode.defaultMode, 150, false)
print("done")
