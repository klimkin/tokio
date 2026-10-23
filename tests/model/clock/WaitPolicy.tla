------------------------------ MODULE WaitPolicy -----------------------------
EXTENDS Integers, TLC
CONSTANT RemoveFloor
VARIABLE sample
Min(a, b) == IF a < b THEN a ELSE b
Max(a, b) == IF a > b THEN a ELSE b
Policy(state, duration, cap) ==
    LET floor == IF ~RemoveFloor /\ state = "Inhibited" /\ duration > 0
                 THEN Max(2, duration) ELSE duration
    IN IF cap = 3 THEN floor ELSE Min(floor, cap)
InitialSample == [state |-> "Running", duration |-> 0, cap |-> 3]
Init == sample = InitialSample
Next == /\ sample = InitialSample
        /\ sample' \in [state : {"Running", "Paused", "Inhibited"},
                          duration : {0, 1, 2, 8}, cap : {3, 0, 1, 2}]
Spec == Init /\ [][Next]_sample
Effective == Policy(sample.state, sample.duration, sample.cap)
ZeroPreserved == sample.duration = 0 => Effective = 0
CapDominates == sample.cap # 3 => Effective <= sample.cap
Precision == sample.state # "Inhibited" =>
             Effective = IF sample.cap = 3 THEN sample.duration ELSE Min(sample.duration, sample.cap)
PositiveInhibitedFloor == (sample.state = "Inhibited" /\ sample.duration > 0 /\ sample.cap = 3)
                         => Effective >= 2
=============================================================================
