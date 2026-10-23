---------------------------- MODULE ClockProtocol ----------------------------
EXTENDS Integers, FiniteSets, TLC
CONSTANTS Operation, Window, Mutation, Callers, Rescue, Spurious, Cap, NeedTimer, AdvanceAmount
VARIABLE p
Slots == 1..Callers
Deadline == 2
Min(a, b) == IF a < b THEN a ELSE b
W == INSTANCE WaitPolicy WITH RemoveFloor <- FALSE, sample <- p
State(mode, count) == IF mode = "Running" THEN "Running"
                      ELSE IF count = 0 THEN "Paused" ELSE "Inhibited"
Policy(state, duration, cap) == W!Policy(state, duration, cap)
InitialCount == IF Operation = "Release" THEN Callers
                ELSE IF Window = "Race" THEN 0 ELSE 1
InitialPC == CASE Window = "During" \/ Window = "Serial" -> "Wait"
                 [] Window = "Before" -> "Fast"
                 [] OTHER -> "Read"
Init == p = [mode |-> "Paused", count |-> InitialCount, now |-> 0,
    pc |-> InitialPC, saved |-> State("Paused", InitialCount),
    duration |-> Deadline, effective |-> Policy(State("Paused", InitialCount), Deadline, Cap),
    readEpoch |-> 0, snapshotEpoch |-> 0, observed |-> 0, epoch |-> 0,
    caller |-> [i \in Slots |-> "Mutate"],
    token |-> IF InitialPC = "Wait" THEN "Parked" ELSE "Empty",
    lock |-> FALSE, sleeping |-> InitialPC = "Wait", signaled |-> FALSE,
    flag |-> FALSE, done |-> FALSE, rejected |-> FALSE, inhibitRace |-> FALSE,
    registered |-> FALSE, lost |-> FALSE, before |-> FALSE, during |-> FALSE,
    nested |-> FALSE]
AllDone == \A i \in Slots : p.caller[i] = "Done"
Settled == AllDone /\ p.observed = Callers /\ (~NeedTimer \/ p.done)
MutationReady(i) ==
    /\ p.caller[i] = "Mutate"
    /\ Operation # "Inhibit"
    /\ IF Window = "Serial" /\ i = 2
          THEN p.caller[1] = "Done" /\ p.observed = 1 /\ p.pc = "Wait"
               /\ p.token = "Parked" /\ ~p.signaled
          ELSE TRUE
    /\ IF Window = "Race" THEN p.pc = "Auto" ELSE TRUE
NoWake == Mutation = "NoWake" \/ (Mutation = "NoFinalWake" /\ p.count = 1)
Mutate(i) ==
    /\ MutationReady(i)
    /\ p' = [p EXCEPT
        !.mode = IF Operation = "Resume" THEN "Running" ELSE @,
        !.count = IF Operation = "Release" THEN @ - 1 ELSE @,
        !.now = IF Operation = "Advance" THEN Min(Deadline, @ + AdvanceAmount) ELSE @,
        !.epoch = @ + 1,
        !.caller[i] = IF NoWake THEN "Done" ELSE "Flag",
        !.before = @ \/ ~p.sleeping,
        !.during = @ \/ p.sleeping,
        !.nested = @ \/ (Operation = "Release" /\ p.count = 1 /\ Callers = 2)]
Flag(i) == /\ p.caller[i] = "Flag"
           /\ p' = [p EXCEPT !.flag = TRUE, !.caller[i] = "Swap"]
Swap(i) ==
    /\ p.caller[i] = "Swap"
    /\ p' = [p EXCEPT
        !.token = IF Mutation = "DropToken" /\ p.token = "Empty"
                  THEN "Empty" ELSE "Notified",
        !.lost = @ \/ (Mutation = "DropToken" /\ p.token = "Empty"),
        !.caller[i] = IF p.token = "Parked" THEN "Lock" ELSE "Done"]
Handoff(i) == /\ p.caller[i] = "Lock" /\ ~p.lock
              /\ p' = [p EXCEPT !.caller[i] = "Signal"]
Signal(i) == /\ p.caller[i] = "Signal"
             /\ p' = [p EXCEPT !.caller[i] = "Done", !.signaled = @ \/ p.sleeping]
Caller(i) == Mutate(i) \/ Flag(i) \/ Swap(i) \/ Handoff(i) \/ Signal(i)
CallerRunnable(i) == MutationReady(i) \/ p.caller[i] \in {"Flag", "Swap", "Signal"}
                     \/ (p.caller[i] = "Lock" /\ ~p.lock)
AfterPark == IF p.saved = "Paused" /\ p.duration > 0 THEN "FlagCheck" ELSE "Process"
Read == /\ p.pc = "Read" /\ ~Settled /\ ~p.rejected
        /\ p' = [p EXCEPT !.duration = Deadline - p.now, !.readEpoch = p.epoch,
                          !.observed = IF Operation = "Advance" /\ p.now < Deadline THEN p.epoch ELSE @,
                          !.pc = IF p.now = Deadline THEN "Fast" ELSE "Snapshot",
                          !.effective = IF p.now = Deadline THEN 0 ELSE @]
Snapshot ==
    /\ p.pc = "Snapshot"
    /\ p' = [p EXCEPT !.saved = State(p.mode, p.count), !.snapshotEpoch = p.epoch,
        !.effective = Policy(State(p.mode, p.count), p.duration, Cap),
        !.observed = IF Operation \in {"Resume", "Release"} THEN p.epoch ELSE @,
        !.pc = "Fast"]
Fast ==
    /\ p.pc = "Fast"
    /\ (Window # "Before" \/ p.epoch > 0)
    /\ IF p.token = "Notified"
          THEN p' = [p EXCEPT !.token = "Empty", !.pc = AfterPark]
          ELSE IF p.effective = 0 \/ (p.saved = "Paused" /\ p.duration > 0)
          THEN p' = [p EXCEPT !.pc = AfterPark]
          ELSE p' = [p EXCEPT !.pc = "Arm"]
Arm == /\ p.pc = "Arm"
       /\ IF p.token = "Notified"
             THEN p' = [p EXCEPT !.token = "Empty", !.pc = AfterPark]
             ELSE p' = [p EXCEPT !.token = "Parked", !.lock = TRUE, !.pc = "Begin"]
BeginWait == /\ p.pc = "Begin"
             /\ p' = [p EXCEPT !.lock = FALSE, !.sleeping = TRUE,
                               !.signaled = FALSE, !.pc = "Wait"]
Return == /\ p.pc = "Wait"
          /\ p' = [p EXCEPT !.token = "Empty", !.sleeping = FALSE,
                            !.signaled = FALSE, !.pc = AfterPark]
NotifyReturn == p.signaled /\ Return
TimeoutReturn == Rescue /\ Return
SpuriousReturn == Spurious /\ Return
FlagCheck == /\ p.pc = "FlagCheck"
             /\ p' = [p EXCEPT !.flag = FALSE, !.pc = IF p.flag THEN "Process" ELSE "Auto"]
Auto == /\ p.pc = "Auto"
        /\ p' = [p EXCEPT !.now = IF p.mode = "Paused" THEN Min(Deadline, @ + p.effective) ELSE @,
            !.rejected = @ \/ p.mode = "Running",
            !.inhibitRace = @ \/ (p.mode = "Paused" /\ p.count > 0), !.pc = "Process"]
Process == /\ p.pc = "Process" /\ ~p.rejected
           /\ p' = [p EXCEPT !.done = @ \/ p.now = Deadline,
                    !.observed = IF Operation = "Advance" /\ p.now = Deadline THEN p.epoch ELSE @,
                    !.pc = "Read"]
Register == /\ Window = "Race" /\ Operation = "Inhibit" /\ ~p.registered /\ p.pc = "Auto"
            /\ p' = [p EXCEPT !.count = @ + 1, !.registered = TRUE,
                              !.caller[1] = "Done", !.epoch = 1, !.observed = 1]
Driver == Read \/ Snapshot \/ Fast \/ Arm \/ BeginWait \/ NotifyReturn \/ FlagCheck \/ Auto \/ Process
DriverRunnable == (~Settled /\ ~p.rejected /\ p.pc = "Read")
                  \/ (p.pc \in {"Snapshot", "Arm", "Begin", "FlagCheck", "Auto"})
                  \/ (p.pc = "Fast" /\ (Window # "Before" \/ p.epoch > 0))
                  \/ (p.pc = "Process" /\ ~p.rejected) \/ (p.pc = "Wait" /\ p.signaled)
Next == (\E i \in Slots : Caller(i)) \/ Driver \/ TimeoutReturn \/ SpuriousReturn \/ Register
Spec == Init /\ [][Next]_p /\ (\A i \in Slots : WF_p(Caller(i))) /\ WF_p(Driver)
TypeOK == /\ p.mode \in {"Paused", "Running"} /\ p.count \in 0..2
          /\ p.now \in 0..Deadline /\ p.epoch \in 0..Callers
          /\ p.observed \in 0..p.epoch /\ p.token \in {"Empty", "Parked", "Notified"}
          /\ p.caller \in [Slots -> {"Mutate", "Flag", "Swap", "Lock", "Signal", "Done"}]
          /\ p.lock = (p.pc = "Begin") /\ p.sleeping = (p.pc = "Wait")
AutoSnapshotGate == p.pc = "Auto" => p.saved = "Paused" /\ p.duration > 0
ZeroWaitBypassesAuto == p.duration = 0 => p.pc \notin {"Snapshot", "FlagCheck", "Auto"}
TimerNotEarly == p.done => p.now = Deadline
BackendRetention == ~p.lost
RequestProgress == (p.epoch > p.observed) ~> (p.epoch = p.observed)
TimerProgress == (p.epoch = Callers /\ Operation = "Release") ~> p.done
WitnessBefore == ~p.before
WitnessDuring == ~p.during
WitnessFlagOnly == ~(p.flag /\ p.token = "Empty")
WitnessTokenOnly == ~(~p.flag /\ p.token = "Notified")
WitnessNested == ~p.nested
WitnessRejected == ~p.rejected
WitnessInhibitedAdvance == ~p.inhibitRace
=============================================================================
