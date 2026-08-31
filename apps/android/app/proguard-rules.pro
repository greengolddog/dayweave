# DayWeave currently relies only on AndroidX components with consumer rules.
# Integration-specific keep rules belong next to their adapters when added.

# Rust exports this exact JNI class/method name; release shrinking must preserve the ABI.
-keep,allowoptimization class com.greengolddog.dayweave.scheduler.RustSchedulerNative {
    byte[] process(byte[]);
}
