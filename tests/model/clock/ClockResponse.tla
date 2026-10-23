---------------------------- MODULE ClockResponse ----------------------------
EXTENDS Integers, TLC
CONSTANTS Operation, Window, Mutation, Callers, Rescue, Spurious, Cap, NeedTimer, AdvanceAmount,
          CallerBudget, BackendBudget, DriverBudget
VARIABLES p, o
P == INSTANCE ClockProtocol
vars == <<p, o>>
Bound == Callers * (2 * CallerBudget + 3 * BackendBudget + 9 * DriverBudget) + 1
OldTimeout == Bound + 4
Min(a, b) == IF a < b THEN a ELSE b
DebtLimit(i) == IF p.caller[i] \in {"Mutate", "Flag"} THEN CallerBudget ELSE BackendBudget
Pending == p.epoch > p.observed
DriverReady == P!DriverRunnable \/ (p.pc = "Wait" /\ o.waitAge >= OldTimeout)
Init == /\ P!Init
        /\ o = [age |-> 0, waitAge |-> 0, callerAge |-> [i \in P!Slots |-> 0],
                 driverAge |-> 0, held |-> TRUE, parity |-> FALSE,
                 overlap |-> FALSE, continued |-> FALSE, responded |-> FALSE]
Observe ==
    o' = [o EXCEPT
        !.age = IF ~o.held \/ p'.epoch = p'.observed \/ ~Pending THEN 0 ELSE @,
        !.waitAge = IF p.pc # "Wait" \/ p'.pc # "Wait" THEN 0 ELSE @,
        !.callerAge = [i \in P!Slots |-> IF p'.caller[i] # p.caller[i] THEN 0 ELSE o.callerAge[i]],
        !.driverAge = IF p'.pc # p.pc THEN 0 ELSE @,
        !.overlap = @ \/ (Pending /\ p'.epoch > p.epoch /\ o.age > 0),
        !.responded = @ \/ (o.held /\ Pending /\ o.age > 0 /\ p'.observed >= p.epoch)]
ProtocolStep == /\ ((\E i \in P!Slots : P!Caller(i)) \/ P!Driver
                     \/ P!SpuriousReturn \/ P!Register
                     \/ (o.waitAge >= OldTimeout /\ P!TimeoutReturn))
                /\ Observe
Tick ==
    LET callerDebt == [i \in P!Slots |-> IF P!CallerRunnable(i)
                       THEN Min(DebtLimit(i) + 1, o.callerAge[i] + 1) ELSE 0]
        driverDebt == IF DriverReady THEN Min(DriverBudget + 1, o.driverAge + 1) ELSE 0
        held == o.held /\ (\A i \in P!Slots : callerDebt[i] <= DebtLimit(i))
                      /\ driverDebt <= DriverBudget
    IN /\ UNCHANGED p
       /\ o' = [o EXCEPT
            !.age = IF held /\ Pending THEN Min(Bound + 1, @ + 1) ELSE 0,
            !.waitAge = IF p.sleeping THEN Min(OldTimeout, @ + 1) ELSE 0,
            !.callerAge = IF held THEN callerDebt ELSE [i \in P!Slots |-> 0],
            !.driverAge = IF held THEN driverDebt ELSE 0,
            !.held = held, !.parity = ~@, !.continued = @ \/ ~o.held]
Next == ProtocolStep \/ Tick
Spec == Init /\ [][Next]_vars /\ WF_vars(Tick)
TypeOK == P!TypeOK /\ o.age \in 0..(Bound + 1) /\ o.waitAge \in 0..OldTimeout
ConditionalResponse == o.held => (~Pending \/ o.age <= Bound)
OldestAgePreserved == [][(Pending /\ p'.epoch > p.epoch /\ o.held) => o'.age = o.age]_vars
WitnessResponded == ~o.responded
WitnessOverlap == ~o.overlap
WitnessTimeContinues == ~o.continued
WitnessBeforeOldTimeout == ~(o.held /\ Pending /\ o.age > Bound /\ p.sleeping /\ o.waitAge < OldTimeout)
=============================================================================
