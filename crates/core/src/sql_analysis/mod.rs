//! SQL Analysis Module
//! 
//! This module provides comprehensive SQL query analysis capabilities including:
//! - Advanced normalization using AST parsing
//! - Query complexity analysis and scoring
//! - Metadata extraction and classification
//! - Performance regression detection
//! - Fingerprinting for accurate query grouping

pub mod normalization;
pub mod complexity;
pub mod metadata;
pub mod regression;
pub mod statistics;

#[cfg(test)]
pub mod tests;

pub use normalization::{
    QueryNormalizer, NormalizationResult, 
    LiteralInfo, LiteralType, normalize_query_enhanced, calculate_query_fingerprint
};

pub use complexity::{
    ComplexityAnalyzer, ComplexityScore, ComplexityComponents, ComplexityClass,
    ComplexityBreakdown, JoinInfo, SubqueryInfo, FunctionInfo, ConditionInfo, AggregationInfo
};

pub use metadata::{
    MetadataExtractor, QueryMetadata, QueryOperation, TableReference, ColumnReference,
    FunctionReference, ExecutionPattern, AccessPattern, QueryClassification, PerformanceHint
};

pub use regression::{
    RegressionDetector, RegressionAnalysis, RegressionStatus, MetricRegression,
    PerformanceDataPoint, PerformanceMetric, RegressionSeverity
};