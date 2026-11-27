// User classes, compiled with ROOT's dictionary (ACLiC): user classes
// round-trip between ROOT and rhizoid.
//
// The Rust twins are in crates/rhizoid-io/tests/derive.rs. Every member
// kind the derive supports appears once: a base class, scalars of each
// type, a fixed array, a `//[fN]` array, TString, std::string, vectors of
// numbers and strings, and an embedded object.
//
//   root -b -q -l -e 'gSystem->SetBuildDir("/tmp/aclic", true)' \
//        -e '.L tests/cross/user_classes.C+' -e 'user_write("user.root")'
//   ... -e 'user_dump("user.root")'

#include <TFile.h>
#include <TKey.h>
#include <TNamed.h>
#include <TObject.h>
#include <TString.h>
#include <TSystem.h>
#include <TTree.h>

#include <cstdio>
#include <string>
#include <vector>

class Hit : public TObject {
public:
   Int_t fId = 0;
   Float_t fE[3] = {0, 0, 0};
   ClassDefOverride(Hit, 1)
};

class UserEvent : public TNamed {
public:
   Int_t fRun = 0;
   Long64_t fEvent = 0;
   Double_t fP[4] = {0, 0, 0, 0};
   Bool_t fGood = false;
   UShort_t fFlags = 0;
   Char_t fCode = 0;
   ULong64_t fMask = 0;
   Int_t fN = 0;
   Float_t *fPt = nullptr; //[fN]
   TString fTag;
   std::string fLabel;
   std::vector<Int_t> fIds;
   std::vector<double> fWeights;
   std::vector<std::string> fNames;
   Hit fHit;
   ~UserEvent() override { delete[] fPt; }
   ClassDefOverride(UserEvent, 3)
};

#ifdef __ROOTCLING__
#pragma link C++ class std::vector<Hit>+;
#endif

void user_write(const char *path)
{
   TFile f(path, "RECREATE");
   for (int i = 0; i < 3; ++i) {
      UserEvent ev;
      ev.SetNameTitle(Form("ev%d", i), "a user event");
      ev.fRun = 100 + i;
      ev.fEvent = 1234567890123LL * (i + 1);
      for (int j = 0; j < 4; ++j)
         ev.fP[j] = 0.5 * j + i;
      ev.fGood = i % 2 == 0;
      ev.fFlags = 0xBEEF + i;
      ev.fCode = 'a' + i;
      ev.fMask = 0xFFFFFFFFFFULL << i;
      ev.fN = i + 1;
      ev.fPt = new Float_t[ev.fN];
      for (int j = 0; j < ev.fN; ++j)
         ev.fPt[j] = 10.25f * (j + 1);
      ev.fTag = Form("tag %d", i);
      ev.fLabel = std::string("label ") + std::to_string(i);
      for (int j = 0; j <= i; ++j) {
         ev.fIds.push_back(7 * j);
         ev.fWeights.push_back(0.125 * j);
         ev.fNames.push_back("n" + std::to_string(j));
      }
      ev.fHit.fId = 42 + i;
      ev.fHit.fE[0] = 1.5f;
      ev.fHit.fE[1] = 2.5f + i;
      ev.fHit.fE[2] = -3.0f;
      ev.fHit.SetUniqueID(9);
      ev.Write(Form("ev%d", i));
   }
   f.Close();
}

static void floats(const char *key, const char *field, const double *v, int n)
{
   printf("%s %s", key, field);
   for (int i = 0; i < n; ++i)
      printf(" %.17g", v[i]);
   printf("\n");
}

void user_dump(const char *path)
{
   TFile *f = TFile::Open(path);
   if (!f || f->IsZombie()) {
      printf("FILE error cannot-open\n");
      gSystem->Exit(1);
   }
   printf("FILE recovered %d\n", f->TestBit(TFile::kRecovered) ? 1 : 0);
   for (auto k : *f->GetListOfKeys()) {
      auto key = static_cast<TKey *>(k);
      std::string id = std::string(key->GetName()) + ";" + std::to_string(key->GetCycle());
      auto ev = key->ReadObject<UserEvent>();
      if (!ev) {
         printf("%s error not-a-UserEvent\n", id.c_str());
         continue;
      }
      const char *k_ = id.c_str();
      printf("%s name %s\n", k_, ev->GetName());
      printf("%s title %s\n", k_, ev->GetTitle());
      printf("%s run %d\n", k_, ev->fRun);
      printf("%s event %lld\n", k_, ev->fEvent);
      floats(k_, "p", ev->fP, 4);
      printf("%s good %d\n", k_, ev->fGood ? 1 : 0);
      printf("%s flags %u\n", k_, ev->fFlags);
      printf("%s code %d\n", k_, ev->fCode);
      printf("%s mask %llu\n", k_, ev->fMask);
      std::vector<double> pt(ev->fPt, ev->fPt + ev->fN);
      floats(k_, "pt", pt.data(), ev->fN);
      printf("%s tag %s\n", k_, ev->fTag.Data());
      printf("%s label %s\n", k_, ev->fLabel.c_str());
      std::vector<double> ids(ev->fIds.begin(), ev->fIds.end());
      floats(k_, "ids", ids.data(), ids.size());
      floats(k_, "weights", ev->fWeights.data(), ev->fWeights.size());
      std::string names;
      for (auto &n : ev->fNames)
         names += (names.empty() ? "" : " ") + n;
      printf("%s names %s\n", k_, names.c_str());
      printf("%s hit.id %d\n", k_, ev->fHit.fId);
      std::vector<double> e(ev->fHit.fE, ev->fHit.fE + 3);
      floats(k_, "hit.e", e.data(), 3);
      printf("%s hit.uniqueid %u\n", k_, ev->fHit.GetUniqueID());
   }
}

// A tree of user classes, split as ROOT splits them (user classes in
// trees round-trip between ROOT and rhizoid): a leaf-list
// branch `run`, a UserEvent branch `event.`, a Hit branch `hit.` and a
// `std::vector<Hit>` branch `hits`, 5 entries.
static UserEvent make_event(int i)
{
   UserEvent ev;
   ev.SetNameTitle(Form("ev%d", i), "a user event");
   ev.fRun = 100 + i;
   ev.fEvent = 1234567890123LL * (i + 1);
   for (int j = 0; j < 4; ++j)
      ev.fP[j] = 0.5 * j + i;
   ev.fGood = i % 2 == 0;
   ev.fFlags = 0xBEEF + i;
   ev.fCode = 'a' + i;
   ev.fMask = 0xFFFFFFFFFFULL << i;
   ev.fN = i % 3;
   ev.fPt = ev.fN ? new Float_t[ev.fN] : nullptr;
   for (int j = 0; j < ev.fN; ++j)
      ev.fPt[j] = 10.25f * (j + 1);
   ev.fTag = Form("tag %d", i);
   ev.fLabel = std::string("label ") + std::to_string(i);
   for (int j = 0; j < i % 4; ++j) {
      ev.fIds.push_back(7 * j);
      ev.fWeights.push_back(0.125 * j);
      ev.fNames.push_back("n" + std::to_string(j));
   }
   ev.fHit.fId = 42 + i;
   ev.fHit.fE[0] = 1.5f;
   ev.fHit.fE[1] = 2.5f + i;
   ev.fHit.fE[2] = -3.0f;
   ev.fHit.SetUniqueID(9);
   return ev;
}

void user_tree_write(const char *path)
{
   TFile f(path, "RECREATE");
   TTree t("t", "user classes");
   Int_t run = 0;
   auto ev = new UserEvent;
   auto hit = new Hit;
   std::vector<Hit> hits;
   t.Branch("run", &run, "run/I");
   t.Branch("event.", &ev, 32000, 99);
   t.Branch("hit.", &hit, 32000, 99);
   t.Branch("hits", &hits, 32000, 99);
   for (int i = 0; i < 5; ++i) {
      run = 7 * i;
      hit->fId = 1000 + i;
      hit->fE[0] = i;
      hit->fE[1] = 2 * i;
      hit->fE[2] = 3 * i;
      hit->SetUniqueID(i);
      UserEvent e = make_event(i);
      ev->SetNameTitle(e.GetName(), e.GetTitle());
      ev->fRun = e.fRun;
      ev->fEvent = e.fEvent;
      for (int j = 0; j < 4; ++j)
         ev->fP[j] = e.fP[j];
      ev->fGood = e.fGood;
      ev->fFlags = e.fFlags;
      ev->fCode = e.fCode;
      ev->fMask = e.fMask;
      delete[] ev->fPt;
      ev->fN = e.fN;
      ev->fPt = ev->fN ? new Float_t[ev->fN] : nullptr;
      for (int j = 0; j < ev->fN; ++j)
         ev->fPt[j] = e.fPt[j];
      ev->fTag = e.fTag;
      ev->fLabel = e.fLabel;
      ev->fIds = e.fIds;
      ev->fWeights = e.fWeights;
      ev->fNames = e.fNames;
      ev->fHit = e.fHit;
      ev->fHit.SetUniqueID(9);
      hits.clear();
      for (int j = 0; j < i; ++j) {
         Hit h;
         h.fId = 10 * i + j;
         h.fE[0] = j;
         h.fE[1] = 0.5f * j;
         h.fE[2] = -j;
         hits.push_back(h);
      }
      t.Fill();
   }
   t.Write();
   f.Close();
}

// What ROOT reads from such a tree, entry by entry, in user_dump's format
// (key `entry/i`, then `hits/i/j` lines).
void user_tree_dump(const char *path)
{
   TFile *f = TFile::Open(path);
   if (!f || f->IsZombie()) {
      printf("FILE error cannot-open\n");
      gSystem->Exit(1);
   }
   printf("FILE recovered %d\n", f->TestBit(TFile::kRecovered) ? 1 : 0);
   auto t = f->Get<TTree>("t");
   Int_t run = 0;
   UserEvent *ev = nullptr;
   Hit *hit = nullptr;
   std::vector<Hit> *hits = nullptr;
   t->SetBranchAddress("run", &run);
   t->SetBranchAddress("hit.", &hit);
   t->SetBranchAddress("event.", &ev);
   t->SetBranchAddress("hits", &hits);
   printf("TREE entries %lld\n", t->GetEntries());
   for (Long64_t i = 0; i < t->GetEntries(); ++i) {
      t->GetEntry(i);
      std::string id = "entry/" + std::to_string(i);
      const char *k_ = id.c_str();
      printf("%s plain.run %d\n", k_, run);
      printf("%s plain.hit.id %d\n", k_, hit->fId);
      std::vector<double> phe(hit->fE, hit->fE + 3);
      floats(k_, "plain.hit.e", phe.data(), 3);
      printf("%s plain.hit.uniqueid %u\n", k_, hit->GetUniqueID());
      printf("%s name %s\n", k_, ev->GetName());
      printf("%s title %s\n", k_, ev->GetTitle());
      printf("%s run %d\n", k_, ev->fRun);
      printf("%s event %lld\n", k_, ev->fEvent);
      floats(k_, "p", ev->fP, 4);
      printf("%s good %d\n", k_, ev->fGood ? 1 : 0);
      printf("%s flags %u\n", k_, ev->fFlags);
      printf("%s code %d\n", k_, ev->fCode);
      printf("%s mask %llu\n", k_, ev->fMask);
      std::vector<double> pt(ev->fPt, ev->fPt + ev->fN);
      floats(k_, "pt", pt.data(), ev->fN);
      printf("%s tag %s\n", k_, ev->fTag.Data());
      printf("%s label %s\n", k_, ev->fLabel.c_str());
      std::vector<double> ids(ev->fIds.begin(), ev->fIds.end());
      floats(k_, "ids", ids.data(), ids.size());
      floats(k_, "weights", ev->fWeights.data(), ev->fWeights.size());
      std::string names;
      for (auto &n : ev->fNames)
         names += (names.empty() ? "" : " ") + n;
      printf("%s names %s\n", k_, names.c_str());
      printf("%s hit.id %d\n", k_, ev->fHit.fId);
      std::vector<double> e(ev->fHit.fE, ev->fHit.fE + 3);
      floats(k_, "hit.e", e.data(), 3);
      printf("%s hit.uniqueid %u\n", k_, ev->fHit.GetUniqueID());
      printf("%s hits %zu\n", k_, hits->size());
      for (size_t j = 0; j < hits->size(); ++j) {
         auto &h = (*hits)[j];
         std::string hid = "hits/" + std::to_string(i) + "/" + std::to_string(j);
         printf("%s id %d\n", hid.c_str(), h.fId);
         std::vector<double> he(h.fE, h.fE + 3);
         floats(hid.c_str(), "e", he.data(), 3);
      }
   }
}
