import Darwin
import Foundation

/// This process's CPU and memory, for the once-a-second stats lines.
public final class ResourceUsage {
    private var lastCPU: Double = ResourceUsage.cpuSeconds()
    private var lastWall: Double = Date().timeIntervalSince1970

    public init() {}

    /// CPU use since the last call, as % of one core; memory footprint in MB (what Activity Monitor shows).
    public func sample() -> (cpuPercent: Double, memoryMB: Double) {
        let cpu = ResourceUsage.cpuSeconds()
        let wall = Date().timeIntervalSince1970
        let percent = wall > lastWall ? (cpu - lastCPU) / (wall - lastWall) * 100 : 0
        lastCPU = cpu
        lastWall = wall
        return (percent, ResourceUsage.footprintMB())
    }

    private static func cpuSeconds() -> Double {
        var usage = rusage()
        getrusage(RUSAGE_SELF, &usage)
        func secs(_ t: timeval) -> Double { Double(t.tv_sec) + Double(t.tv_usec) / 1_000_000 }
        return secs(usage.ru_utime) + secs(usage.ru_stime)
    }

    private static func footprintMB() -> Double {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let result = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        return result == KERN_SUCCESS ? Double(info.phys_footprint) / 1_048_576 : 0
    }
}
