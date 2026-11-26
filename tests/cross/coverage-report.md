# Coverage against ROOT

Every branch of numbers in 58 files, as ROOT 6.42 reads it (entry by entry, and through `TTree::Draw`: the number of values and their sum, arrays and collections flattened; a match with either is a match) and as rhizoid reads it (`tests/coverage.rs`, `tests/cross/root_sums.C`).

**2130 of 2130 branches ROOT reads match (100.0 %)**; 0 differ; 0 are not read by rhizoid.

| file | branches ROOT reads | match | differ | unsupported | not read by ROOT | ROOT crashes |
|---|---|---|---|---|---|---|
| Run2012BC_DoubleMuParked_Muons_1000evts_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| cmsopendata2015_ttbar_19980_NANOAOD_RNTupleImporter_rntuple_v1-0-0-1.root | 0 | 0 | 0 | 0 | 0 | 0 |
| ntpl001_staff_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| ntpl001_staff_rntuple_v1-0-1-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| rntviewer-testfile-multiple-rntuples-v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| rntviewer-testfile-uncomp-single-rntuple-v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_1jag_int_float_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_atomic_bitset_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_bit_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_class_inheritance_rntuple_v1-0-0-1.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_emptystruct_invalidvar_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_extension_columns_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_float_types_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_index_multicluster_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_int_5e4_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_int_float_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_int_multicluster_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_int_vfloat_tlv_vtlv_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_multiple_cluster_groups_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_multiple_representations_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_nested_structs_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_split_3e4_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_splitint_rntuple_v1-0-1-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| test_stl_containers_rntuple_v1-0-0-0.root | 0 | 0 | 0 | 0 | 0 | 0 |
| uproot-Event.root | 47 | 47 | 0 | 0 | 8 | 0 |
| uproot-HZZ-objects.root | 11 | 11 | 0 | 0 | 11 | 0 |
| uproot-HZZ.root | 51 | 51 | 0 | 0 | 0 | 0 |
| uproot-Zmumu.root | 19 | 19 | 0 | 0 | 1 | 0 |
| uproot-demo-double32.root | 25 | 25 | 0 | 0 | 0 | 0 |
| uproot-double32-float16.root | 12 | 12 | 0 | 0 | 0 | 0 |
| uproot-foriter.root | 1 | 1 | 0 | 0 | 0 | 0 |
| uproot-issue21.root | 7 | 7 | 0 | 0 | 0 | 0 |
| uproot-leaflist.root | 0 | 0 | 0 | 0 | 1 | 0 |
| uproot-mc10events.root | 23 | 23 | 0 | 0 | 5 | 200 |
| uproot-nesteddirs.root | 57 | 57 | 0 | 0 | 7 | 0 |
| uproot-physlite-rntuple_v1-0-0-0.root | 1575 | 1575 | 0 | 0 | 2005 | 0 |
| uproot-sample-5.23.02-zlib.root | 34 | 34 | 0 | 0 | 1 | 0 |
| uproot-sample-5.30.00-zlib.root | 34 | 34 | 0 | 0 | 1 | 0 |
| uproot-sample-6.08.04-lzma.root | 34 | 34 | 0 | 0 | 1 | 0 |
| uproot-sample-6.20.04-lz4.root | 34 | 34 | 0 | 0 | 1 | 0 |
| uproot-sample-6.20.04-zlib.root | 34 | 34 | 0 | 0 | 1 | 0 |
| uproot-simple.root | 2 | 2 | 0 | 0 | 1 | 0 |
| uproot-small-dy-nooffsets.root | 8 | 8 | 0 | 0 | 0 | 0 |
| uproot-small-dy-withoffsets.root | 8 | 8 | 0 | 0 | 0 | 0 |
| uproot-small-evnt-tree-fullsplit.root | 36 | 36 | 0 | 0 | 5 | 0 |
| uproot-small-evnt-tree-nosplit.root | 0 | 0 | 0 | 0 | 1 | 0 |
| uproot-small-flat-tree.root | 19 | 19 | 0 | 0 | 1 | 0 |
| uproot-stl_containers.root | 1 | 1 | 0 | 0 | 25 | 0 |
| uproot-vectorVectorDouble.root | 0 | 0 | 0 | 0 | 1 | 0 |
| root_dirs.root | 0 | 0 | 0 | 0 | 0 | 0 |
| root_hists.root | 0 | 0 | 0 | 0 | 0 | 0 |
| root_ntuple.root | 0 | 0 | 0 | 0 | 0 | 0 |
| root_tree.root | 14 | 14 | 0 | 0 | 1 | 0 |
| root_user.root | 0 | 0 | 0 | 0 | 0 | 0 |
| root_user_tree.root | 27 | 27 | 0 | 0 | 5 | 0 |
| uproot_hists.root | 0 | 0 | 0 | 0 | 0 | 0 |
| uproot_rntuple.root | 0 | 0 | 0 | 0 | 0 | 0 |
| uproot_trees.root | 17 | 17 | 0 | 0 | 0 | 0 |

## Not covered

Branches ROOT does not read as numbers (by reason):

- leaves=3: 1
- no reading: ROOT's leaf API fails without the class's dictionary: 2
- type BASE: 203
- type DataHeaderForm_p6: 1
- type DataHeader_p6: 1
- type DataVector<xAOD::TriggerMenuJson_v1>: 6
- type DataVector<xAOD::TruthMetaData_v1>: 1
- type Event: 1
- type EventStreamInfo_p3: 1
- type IOVMetaDataContainer_p1: 4
- type TH1F*: 1
- type TRef: 2
- type TRefArray*: 2
- type TString: 10
- type TVector2: 1
- type TVector3: 6
- type bitset<256>: 5
- type char*: 1
- type map<int,set<short> >: 1
- type map<int,set<string> >: 1
- type map<int,short>: 1
- type map<int,vector<set<short> > >: 1
- type map<int,vector<short> >: 1
- type map<int,vector<string> >: 1
- type map<int,vector<vector<short> > >: 1
- type map<string,TString>: 1
- type map<string,set<short> >: 1
- type map<string,set<string> >: 1
- type map<string,short>: 1
- type map<string,string>: 1
- type map<string,vector<short> >: 1
- type map<string,vector<string> >: 1
- type set<int>: 1
- type set<string>: 1
- type string: 27
- type vector<ElementLink<xAOD::CutBookkeeperContainer_v1> >: 195
- type vector<TLorentzVector>: 4
- type vector<TString>: 1
- type vector<set<int> >: 1
- type vector<set<string> >: 1
- type vector<string>: 607
- type vector<vector<ElementLink<xAOD::CutBookkeeperContainer_v1> > >: 585
- type vector<vector<TString> >: 1
- type vector<vector<double> >: 1
- type vector<vector<int> >: 1
- type vector<vector<string> >: 197
- type xAOD::CutBookkeeperContainer_v1: 195
- type xAOD::EventFormat_v1: 1
- type xAOD::FileMetaData_v1: 1

Branches ROOT crashes on (classes without a dictionary), 200:

- uproot-mc10events.root: Events/AK4CHS.area
- uproot-mc10events.root: Events/AK4CHS.bmva
- uproot-mc10events.root: Events/AK4CHS.csv
- uproot-mc10events.root: Events/AK4CHS.d0
- uproot-mc10events.root: Events/AK4CHS.dz
- uproot-mc10events.root: Events/AK4CHS.eta
- uproot-mc10events.root: Events/AK4CHS.mass
- uproot-mc10events.root: Events/AK4CHS.phi
- uproot-mc10events.root: Events/AK4CHS.pt
- uproot-mc10events.root: Events/AK4CHS.ptRaw
- uproot-mc10events.root: Events/AK4CHS.unc
- uproot-mc10events.root: Events/Electron
- uproot-mc10events.root: Events/Electron.chHadIso
- uproot-mc10events.root: Events/Electron.classification
- uproot-mc10events.root: Events/Electron.d0
- uproot-mc10events.root: Events/Electron.dEtaIn
- uproot-mc10events.root: Events/Electron.dEtaInSeed
- uproot-mc10events.root: Events/Electron.dPhiIn
- uproot-mc10events.root: Events/Electron.dz
- uproot-mc10events.root: Events/Electron.e1x5
- uproot-mc10events.root: Events/Electron.e2x5
- uproot-mc10events.root: Events/Electron.e5x5
- uproot-mc10events.root: Events/Electron.ecalEnergy
- uproot-mc10events.root: Events/Electron.ecalIso
- uproot-mc10events.root: Events/Electron.ecalPFClusIso
- uproot-mc10events.root: Events/Electron.eoverp
- uproot-mc10events.root: Events/Electron.eta
- uproot-mc10events.root: Events/Electron.fbrem
- uproot-mc10events.root: Events/Electron.fiducialBits
- uproot-mc10events.root: Events/Electron.gammaIso
- uproot-mc10events.root: Events/Electron.hcalDepth1Iso
- uproot-mc10events.root: Events/Electron.hcalIso
- uproot-mc10events.root: Events/Electron.hcalPFClusIso
- uproot-mc10events.root: Events/Electron.hovere
- uproot-mc10events.root: Events/Electron.isConv
- uproot-mc10events.root: Events/Electron.mva
- uproot-mc10events.root: Events/Electron.nMissingHits
- uproot-mc10events.root: Events/Electron.neuHadIso
- uproot-mc10events.root: Events/Electron.pfEta
- uproot-mc10events.root: Events/Electron.pfPhi
- uproot-mc10events.root: Events/Electron.pfPt
- uproot-mc10events.root: Events/Electron.phi
- uproot-mc10events.root: Events/Electron.pt
- uproot-mc10events.root: Events/Electron.puIso
- uproot-mc10events.root: Events/Electron.puppiChHadIso
- uproot-mc10events.root: Events/Electron.puppiChHadIsoNoLep
- uproot-mc10events.root: Events/Electron.puppiGammaIso
- uproot-mc10events.root: Events/Electron.puppiGammaIsoNoLep
- uproot-mc10events.root: Events/Electron.puppiNeuHadIso
- uproot-mc10events.root: Events/Electron.puppiNeuHadIsoNoLep
- uproot-mc10events.root: Events/Electron.q
- uproot-mc10events.root: Events/Electron.r9
- uproot-mc10events.root: Events/Electron.scEt
- uproot-mc10events.root: Events/Electron.scEta
- uproot-mc10events.root: Events/Electron.scID
- uproot-mc10events.root: Events/Electron.scPhi
- uproot-mc10events.root: Events/Electron.sieie
- uproot-mc10events.root: Events/Electron.sip3d
- uproot-mc10events.root: Events/Electron.trkID
- uproot-mc10events.root: Events/Electron.trkIso
- uproot-mc10events.root: Events/Electron.typeBits
- uproot-mc10events.root: Events/Muon
- uproot-mc10events.root: Events/Muon.caloComp
- uproot-mc10events.root: Events/Muon.chHadIso
- uproot-mc10events.root: Events/Muon.chi2LocPos
- uproot-mc10events.root: Events/Muon.d0
- uproot-mc10events.root: Events/Muon.dz
- uproot-mc10events.root: Events/Muon.ecalIso
- uproot-mc10events.root: Events/Muon.eta
- uproot-mc10events.root: Events/Muon.gammaIso
- uproot-mc10events.root: Events/Muon.glbKink
- uproot-mc10events.root: Events/Muon.hcalIso
- uproot-mc10events.root: Events/Muon.muNchi2
- uproot-mc10events.root: Events/Muon.nMatchStn
- uproot-mc10events.root: Events/Muon.nPixHits
- uproot-mc10events.root: Events/Muon.nPixLayers
- uproot-mc10events.root: Events/Muon.nTkHits
- uproot-mc10events.root: Events/Muon.nTkLayers
- uproot-mc10events.root: Events/Muon.nValidHits
- uproot-mc10events.root: Events/Muon.neuHadIso
- uproot-mc10events.root: Events/Muon.pfEta
- uproot-mc10events.root: Events/Muon.pfPhi
- uproot-mc10events.root: Events/Muon.pfPt
- uproot-mc10events.root: Events/Muon.phi
- uproot-mc10events.root: Events/Muon.pogIDBits
- uproot-mc10events.root: Events/Muon.pt
- uproot-mc10events.root: Events/Muon.ptErr
- uproot-mc10events.root: Events/Muon.puIso
- uproot-mc10events.root: Events/Muon.puppiChHadIso
- uproot-mc10events.root: Events/Muon.puppiChHadIsoNoLep
- uproot-mc10events.root: Events/Muon.puppiGammaIso
- uproot-mc10events.root: Events/Muon.puppiGammaIsoNoLep
- uproot-mc10events.root: Events/Muon.puppiNeuHadIso
- uproot-mc10events.root: Events/Muon.puppiNeuHadIsoNoLep
- uproot-mc10events.root: Events/Muon.q
- uproot-mc10events.root: Events/Muon.segComp
- uproot-mc10events.root: Events/Muon.selectorBits
- uproot-mc10events.root: Events/Muon.sip3d
- uproot-mc10events.root: Events/Muon.staEta
- uproot-mc10events.root: Events/Muon.staPhi
- uproot-mc10events.root: Events/Muon.staPt
- uproot-mc10events.root: Events/Muon.tkNchi2
- uproot-mc10events.root: Events/Muon.trkHitFrac
- uproot-mc10events.root: Events/Muon.trkID
- uproot-mc10events.root: Events/Muon.trkIso
- uproot-mc10events.root: Events/Muon.trkKink
- uproot-mc10events.root: Events/Muon.typeBits
- uproot-mc10events.root: Events/Photon
- uproot-mc10events.root: Events/Photon.chHadIso
- uproot-mc10events.root: Events/Photon.ecalIso
- uproot-mc10events.root: Events/Photon.eta
- uproot-mc10events.root: Events/Photon.fiducialBits
- uproot-mc10events.root: Events/Photon.gammaIso
- uproot-mc10events.root: Events/Photon.hasPixelSeed
- uproot-mc10events.root: Events/Photon.hcalIso
- uproot-mc10events.root: Events/Photon.hovere
- uproot-mc10events.root: Events/Photon.isConv
- uproot-mc10events.root: Events/Photon.mva
- uproot-mc10events.root: Events/Photon.neuHadIso
- uproot-mc10events.root: Events/Photon.passElectronVeto
- uproot-mc10events.root: Events/Photon.phi
- uproot-mc10events.root: Events/Photon.pt
- uproot-mc10events.root: Events/Photon.r9
- uproot-mc10events.root: Events/Photon.scEt
- uproot-mc10events.root: Events/Photon.scEta
- uproot-mc10events.root: Events/Photon.scID
- uproot-mc10events.root: Events/Photon.scPhi
- uproot-mc10events.root: Events/Photon.sieie
- uproot-mc10events.root: Events/Photon.sipip
- uproot-mc10events.root: Events/Photon.sthovere
- uproot-mc10events.root: Events/Photon.trkIso
- uproot-mc10events.root: Events/Photon.typeBits
- uproot-mc10events.root: Events/Tau
- uproot-mc10events.root: Events/Tau.antiEleMVA5
- uproot-mc10events.root: Events/Tau.antiEleMVA5Cat
- uproot-mc10events.root: Events/Tau.dzLeadChHad
- uproot-mc10events.root: Events/Tau.e
- uproot-mc10events.root: Events/Tau.eta
- uproot-mc10events.root: Events/Tau.hpsDisc
- uproot-mc10events.root: Events/Tau.m
- uproot-mc10events.root: Events/Tau.nSignalChHad
- uproot-mc10events.root: Events/Tau.nSignalGamma
- uproot-mc10events.root: Events/Tau.phi
- uproot-mc10events.root: Events/Tau.pt
- uproot-mc10events.root: Events/Tau.puppiChHadIso
- uproot-mc10events.root: Events/Tau.puppiChHadIsoNoLep
- uproot-mc10events.root: Events/Tau.puppiGammaIso
- uproot-mc10events.root: Events/Tau.puppiGammaIsoNoLep
- uproot-mc10events.root: Events/Tau.puppiNeuHadIso
- uproot-mc10events.root: Events/Tau.puppiNeuHadIsoNoLep
- uproot-mc10events.root: Events/Tau.q
- uproot-mc10events.root: Events/Tau.rawIso3Hits
- uproot-mc10events.root: Events/Tau.rawIsoMVA3newDMwLT
- uproot-mc10events.root: Events/Tau.rawIsoMVA3newDMwoLT
- uproot-mc10events.root: Events/Tau.rawIsoMVA3oldDMwLT
- uproot-mc10events.root: Events/Tau.rawIsoMVA3oldDMwoLT
- uproot-mc10events.root: Events/Tau.rawMuonRejection
- uproot-mc10events.root: Events/alpacaMET
- uproot-mc10events.root: Events/alpacaMETphi
- uproot-mc10events.root: Events/bsx
- uproot-mc10events.root: Events/bsy
- uproot-mc10events.root: Events/bsz
- uproot-mc10events.root: Events/evtNum
- uproot-mc10events.root: Events/hasGoodPV
- uproot-mc10events.root: Events/lumiSec
- uproot-mc10events.root: Events/metFilterFailBits
- uproot-mc10events.root: Events/nPU
- uproot-mc10events.root: Events/nPUm
- uproot-mc10events.root: Events/nPUmean
- uproot-mc10events.root: Events/nPUmeanm
- uproot-mc10events.root: Events/nPUmeanp
- uproot-mc10events.root: Events/nPUp
- uproot-mc10events.root: Events/pcpMET
- uproot-mc10events.root: Events/pcpMETphi
- uproot-mc10events.root: Events/pfMET
- uproot-mc10events.root: Events/pfMETC
- uproot-mc10events.root: Events/pfMETCCov00
- uproot-mc10events.root: Events/pfMETCCov01
- uproot-mc10events.root: Events/pfMETCCov11
- uproot-mc10events.root: Events/pfMETCov00
- uproot-mc10events.root: Events/pfMETCov01
- uproot-mc10events.root: Events/pfMETCov11
- uproot-mc10events.root: Events/pfMETCphi
- uproot-mc10events.root: Events/pfMETphi
- uproot-mc10events.root: Events/puppET
- uproot-mc10events.root: Events/puppETC
- uproot-mc10events.root: Events/puppETCCov00
- uproot-mc10events.root: Events/puppETCCov01
- uproot-mc10events.root: Events/puppETCCov11
- uproot-mc10events.root: Events/puppETCov00
- uproot-mc10events.root: Events/puppETCov01
- uproot-mc10events.root: Events/puppETCov11
- uproot-mc10events.root: Events/puppETCphi
- uproot-mc10events.root: Events/puppETphi
- uproot-mc10events.root: Events/pvx
- uproot-mc10events.root: Events/pvy
- uproot-mc10events.root: Events/pvz
- uproot-mc10events.root: Events/rhoIso
- uproot-mc10events.root: Events/rhoJet
- uproot-mc10events.root: Events/runNum

Differ, 0:


Not read by rhizoid, 0:

