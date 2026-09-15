# Implementation details for AS5438

## Overview

The AS5438 standard document (in draft as of 2026-08-31) describes the required
parameters for an inverter system to be interoperable with the energy grid. It
also provides explicit mappings for the CSIP-AUS, SunSpec Modbus and OCPP
protocols for the set of parameters.

In this crate, these mappings are used to translate parameters from CSIP-AUS to
SunSpec Modbus. However, some parameters are only loosely coupled and either require
some additional parameters to be set, or a decision on how to apply the values. This
file documents the choices made in this crate.

## Scaling factors (Tables E.1-12)

The SunSpec models contain scaling factors for many parameters. While not
explicitly mentioned in AS5438, it is a requirement that the modbus client reads
the scaling factors to interpret values obtained from reads and convert values
to write into the correct scale.

## Curve group assignments (Tables E.4, E.6-8)

When assigning to a curve, there is a list of curve or curve set options in a
SunSpec model with up to `NCrv`/`NCrvSet` curves available. Each curve can
support up to `NPt` points. The bridge service will read these two constants
when determining how to apply a curve. If `NPt` is less than the length of the
curve to be applied, then the bridge service will not apply the curve at all and
discard it.

If a model supports curves in the SunSpec definition, then it should also allow
at least 2 curves in the model. The first is for the active applied settings and
is readonly to the modbus client. The bridge service will not assume `NCrv >= 2`
however, and will test this constant, discarding the curve if not enough curves are
supported by the device. The bridge service will always write to the curve in
index 2 (the second curve).

An assignment of a curve to a SunSpec device also requires an assignment of
`ActPt`, the number of active points in the curve, which is allowed to be less
than `NPt`. The bridge service assigns the x,y points up to `ActPt` and does not
touch the registers past this limit which retain whatever values they had before.

After assignment is complete, the bridge service writes `2` to `AdptCrvReq`
which requests the device take on the values in curve index 2. The bridge
service always uses index 2 and assumes that the device will either update its
settings based on updated values in the curve data points, or in response to a
write to `AdptCrvReq`: hence it will write 2, even if it has previously written
2 to `AdptCrvReq`.

A pedantic comment about the tables in AS5438: the points mentioned vary from
table to table in the length of the curve and the choice of index. It is assumed
that AS5438 simply means "a set of curve data points with x and y values" for
each of these tables.

## DeptRef of Tables E.4 and E.6 (Sections E.4.2 and E.4.4)

These two tables refer to models 705 and 706. In these curves, the dependent
axis (the y axis) is a percentage and is supposed to be assigned relative to the
dependent reference, point `DEPT_REF`. The tables of AS5438 do not mention this,
so we choose to map the SEP2 values for `DERCurve::y_ref_type` to `DEPT_REF`.

## Table E.4 (Section E.4.2)

This table references `705.VRef`, `705.VRefAutoEna`, `705.VRefAutoTms` and
`705.RspTms` as though they are model parameters. These are instead specific to
the curve SunSpec group and sent along with the curve data in curve 2.

## Table E.6 (Section E.4.4)

This table references model 706 for one point but 705 for the others. It is
assumed this is a typo and the curve applies only to model 706.

## Tables E.7 and E.8 (Sections E.4.5 and E.4.6)

These tables refer to only a subset of curve types. As it is trivial to extend
to all curve types, we include "Must Trip", "May Trip" and "Momentary Cessation"
for all models, when CSIP-AUS also defines that curve.

## Table E.9 (Section E.4.7)

The frequency droop SunSpec group is a repeating group `711.Ctl`. This follows a
similar pattern to the curve group assignment but will be repeated here for
completeness.

The first element of the repeating group is a readonly value for the current
settings of the device. The parameters are chosen to be assigned to the second
element of the repeating group, if the model allows it (`711.NCtl` at least 2).
If the model doesn't allow it, the parameters are dropped.

In addition `711.Ena` is required to be set to enabled/disabled depending on
whether a frequency droop is provided, and `711.AdptCtlReq` is assigned 2 after
the `Ctl` group data has been updated.

## Tables E.5 and E.12 (Sections E.4.3 and E.4.10)

The SunSpec parameters `VarSetMod` and `WSetMod` are not mentioned in the
tables. These parameters are intended to switch between various modes
(percentage or absolute) and different reference frames when percentages are
defined. For `VarSetMod` we attempt to set the value based on `opModFixedVar`'s
`ref_type`. For `WSetMod` we choose to set `WSetMod=W_MAX_PCT` when `WSetPct` is
provided.

We also note that it is unclear how `opModTargetVar` should be translated to the
`VarSetPct` setting, so we only act on changes to `opModFixedVar`.

## Tables F.3-10

In many parameters in these tables, only `DERControl::X` is mentioned. To be
pedantic, these settings exist on the `DERControlBase` object which can be
specified through either a `DERControl` or a `DefaultDERControl`. In this
crate, the appropriate value is derived through layering a set of scheduled
`DERControl`s and a set of `DefaultDERControl`s using the primacy ordering
specified in the CSIP-AUS spec.

## Table F.2 (Section F.3)

The connection status is given two targets, `DERStatus::genConnectStatus` and
`DERStatus::storConnectStatus`. While AS5438 describes these as parameters to be
read, we are using them to report to the CSIP-AUS server which is out of scope
for AS5438. We assign the measured value from SunSpec `701.ConnSt` to both
`DERStatus::genConnectStatus` and `DERStatus::storConnectStatus`.

## Extension parameters

In addition to those specified in the appendices of AS5438 we also support some
more parameters:
- Set active power in Watts: `opModTargetW` -> `WSet`
- Readings of power for 3-phase: (SunSpec `701.WL1`, etc...).
