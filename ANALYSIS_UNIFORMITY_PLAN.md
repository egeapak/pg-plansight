# Analysis Rules Uniformity & Expansion Plan

## 🔍 Current Analysis Issues

### 1. **Inconsistent Threshold Patterns**

**Row Count Thresholds (all over the place):**
- RowEstimation: Critical=1M, High=100K, Medium=50K
- SequentialScan: High=100K, Medium=10K
- NestedLoop: High=10K, Medium=1K
- ParallelOpportunity: Min=50K
- AggregateMemory: Large=1M

**Cost Thresholds (inconsistent scales):**
- SequentialScan: High=10K, Medium=1K
- StartupCost: High=10K, Medium=1K  
- TotalCost: High=50K, Medium=10K
- IndexScan: HighRange=50K, MediumStartup=100

**Ratio Thresholds (somewhat better but still varied):**
- EstimationError: Critical=10x, High=3x, Medium=1x
- Memory: Spill=2.0x, Warning=1.2x (good)
- CartesianProduct: Min=0.5

---

## 🎯 **Proposed Unified Framework**

### **Core Principles:**
1. **Scale-Aware Thresholds**: Different operation types have different natural scales
2. **Consistent Severity Mapping**: Same relative impact → same severity
3. **Logarithmic Progressions**: Use 10x, 100x patterns where appropriate
4. **Context-Sensitive**: Account for operation type and database size

### **Standardized Threshold Tiers**

#### **Row Count Thresholds (by operation type):**
```rust
// Heavy Operations (Scans, Joins)
VeryLarge: 10M+    → Critical
Large: 1M-10M      → High  
Medium: 100K-1M    → Medium
Small: 10K-100K    → Low

// Light Operations (Index lookups, small joins)  
VeryLarge: 1M+     → Critical
Large: 100K-1M     → High
Medium: 10K-100K   → Medium
Small: 1K-10K      → Low

// Memory Operations (Sorts, Hashes)
VeryLarge: 5M+     → Critical (likely spill)
Large: 500K-5M     → High
Medium: 50K-500K   → Medium  
Small: 5K-50K      → Low
```

#### **Cost Thresholds (PostgreSQL cost units):**
```rust
// Absolute Cost Thresholds
Extreme: 1M+       → Critical
High: 100K-1M      → High
Medium: 10K-100K   → Medium
Low: 1K-10K        → Low

// Startup vs Total Cost Ratios
HighStartup: >50%  → High (startup-heavy)
MedStartup: 20-50% → Medium
LowStartup: <20%   → Good
```

#### **Performance Ratio Thresholds:**
```rust
// Error/Estimation Ratios
Severe: 100x+      → Critical
High: 10x-100x     → High  
Medium: 3x-10x     → Medium
Low: 1.5x-3x       → Low

// Memory Spill Ratios (relative to work_mem)
Critical: 10x+     → Critical
High: 3x-10x       → High
Medium: 1.5x-3x    → Medium
Warning: 1.2x-1.5x → Low

// Efficiency Ratios (parallelism, selectivity)
Poor: <20%         → High
Fair: 20-50%       → Medium  
Good: 50-80%       → Low
Excellent: >80%    → Info
```

---

## 🚀 **New Analysis Capabilities**

### **1. Temporal Pattern Analysis**
```rust
pub struct TemporalAnalyzer {
    // Time-based patterns
    pub peak_detection: PeakDetectionConfig,
    pub trend_analysis: TrendAnalysisConfig,
    pub anomaly_detection: AnomalyDetectionConfig,
}

// Findings:
- PeakHourBottleneck: Query performs 10x worse during peak hours
- WeekendAnomaly: Unusual performance pattern on weekends  
- GradualDegradation: Performance declining over time
- SpikeDetection: Sudden performance spike detected
```

### **2. Resource Utilization Analysis**
```rust
pub struct ResourceAnalyzer {
    // System resource patterns
    pub buffer_analysis: BufferAnalysisConfig,
    pub io_analysis: IOAnalysisConfig, 
    pub cpu_analysis: CPUAnalysisConfig,
}

// Findings:
- LowBufferHitRatio: Buffer hit ratio below 95%
- ExcessiveIO: I/O wait dominates query time
- CPUBottleneck: Query is CPU-bound vs I/O-bound
- MemoryPressure: System memory constraints detected
```

### **3. Query Pattern Analysis**
```rust
pub struct QueryPatternAnalyzer {
    // Query behavior patterns
    pub similarity_detection: SimilarityConfig,
    pub antipattern_detection: AntiPatternConfig,
    pub complexity_scoring: ComplexityConfig,
}

// Findings:
- NPlusOnePattern: Multiple similar queries detected
- CartesianJoinPattern: Accidental cartesian products
- OverComplexQuery: Query complexity score > threshold
- DuplicateQueries: Identical queries with different parameters
```

### **4. Index Effectiveness Analysis**
```rust
pub struct IndexAnalyzer {
    // Index usage and effectiveness
    pub usage_analysis: IndexUsageConfig,
    pub selectivity_analysis: SelectivityConfig,
    pub maintenance_analysis: MaintenanceConfig,
}

// Findings:
- UnusedIndex: Index created but never used
- PoorSelectivity: Index selectivity below 5%
- MissingIndex: Sequential scan could benefit from index
- IndexFragmentation: Index needs maintenance
```

### **5. Concurrency & Lock Analysis**
```rust
pub struct ConcurrencyAnalyzer {
    // Lock and concurrency patterns
    pub lock_analysis: LockAnalysisConfig,
    pub deadlock_detection: DeadlockConfig,
    pub contention_analysis: ContentionConfig,
}

// Findings:
- LockWaitBottleneck: Excessive lock wait times
- DeadlockPattern: Deadlock-prone query pattern
- HighContention: Multiple queries competing for same resource
- SerializationFailure: Transaction conflicts detected
```

### **6. Data Distribution Analysis**
```rust
pub struct DistributionAnalyzer {
    // Data skew and distribution
    pub skew_detection: SkewDetectionConfig,
    pub partition_analysis: PartitionConfig,
    pub hotspot_detection: HotspotConfig,
}

// Findings:
- DataSkew: Highly uneven data distribution detected
- InefficientPartitioning: Query scans multiple partitions
- HotPartition: Single partition handling most traffic
- ColdData: Query accessing rarely-used data
```

### **7. Statistics & Maintenance Analysis**
```rust
pub struct MaintenanceAnalyzer {
    // Database maintenance effectiveness
    pub stats_freshness: StatsConfig,
    pub vacuum_analysis: VacuumConfig,
    pub maintenance_timing: MaintenanceTimingConfig,
}

// Findings:
- StaleStatistics: Table statistics over 7 days old
- VacuumOverdue: Table needs vacuum/analyze
- AutoVacuumTuning: Autovacuum settings suboptimal
- MaintenanceWindow: Maintenance impacting performance
```

### **8. Network & Connection Analysis**
```rust
pub struct NetworkAnalyzer {
    // Connection and network patterns
    pub connection_pooling: ConnectionPoolConfig,
    pub network_latency: NetworkLatencyConfig,
    pub client_patterns: ClientPatternConfig,
}

// Findings:
- ConnectionThrashing: Frequent connect/disconnect
- NetworkLatency: High client-server latency detected
- PoolStarvation: Connection pool exhaustion
- ClientBottleneck: Client-side processing delays
```

---

## 📊 **Implementation Priority**

### **Phase 1: Standardize Current (High Priority)**
1. ✅ **Unified Threshold Framework** - Standardize existing thresholds
2. ✅ **Configuration Consistency** - Make all configs follow same pattern
3. ✅ **Severity Mapping** - Ensure consistent severity assignments

### **Phase 2: Enhanced Current (Medium Priority)**  
4. **Temporal Analysis** - Time-based patterns within existing queries
5. **Resource Analysis** - Extend current analysis with resource usage
6. **Query Patterns** - Anti-pattern detection in current analyzers

### **Phase 3: New Capabilities (Low Priority)**
7. **Index Effectiveness** - Requires statistics access
8. **Concurrency Analysis** - Requires lock information
9. **Data Distribution** - Advanced statistical analysis
10. **Maintenance Analysis** - Requires system metadata
11. **Network Analysis** - Requires connection monitoring

---

## 🔧 **Implementation Strategy**

### **1. Create Unified Base Types**
```rust
pub struct UnifiedThresholds {
    pub row_count: RowCountTier,
    pub cost: CostTier, 
    pub ratio: RatioTier,
    pub duration: DurationTier,
}

pub enum OperationType {
    Scan,      // Sequential/Index scans
    Join,      // All join types
    Sort,      // Sort operations  
    Hash,      // Hash operations
    Aggregate, // Grouping/aggregation
    Index,     // Index operations
}
```

### **2. Scale-Aware Configuration**
```rust
impl UnifiedThresholds {
    pub fn for_operation(op_type: OperationType) -> Self {
        match op_type {
            OperationType::Scan => Self::scan_thresholds(),
            OperationType::Join => Self::join_thresholds(),
            // ... etc
        }
    }
}
```

### **3. Context-Sensitive Analysis**
```rust
pub struct AnalysisContext {
    pub database_size: DatabaseSize,
    pub workload_type: WorkloadType,
    pub performance_target: PerformanceTarget,
}
```

This unified approach will make the analysis more consistent, predictable, and easier to tune while providing comprehensive coverage of PostgreSQL performance issues.