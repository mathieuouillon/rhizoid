// Writes, with ROOT itself, objects whose members take the decoder's less
// common paths: an object-wise std::map, a std::bitset, a TRef to a member
// (whose TObject then carries a process-ID index), a TMap, members the
// decoder keeps undecoded (a TBits, a pointer to one, a std::forward_list),
// packed floats, and TLists with and without options. Then a tree of
// unsplit maps, which ROOT streams member-wise. The fixture of
// `tests/members.rs`.
//
//   root -b -q -l -e 'gSystem->SetBuildDir("/tmp/aclic", true)' \
//        -e '.L tests/cross/root_members.C+' -e 'members_write("root_members.root")'

#include <TBits.h>
#include <TFile.h>
#include <TList.h>
#include <TMap.h>
#include <TNamed.h>
#include <TObjArray.h>
#include <TObjString.h>
#include <TObject.h>
#include <TRef.h>
#include <TTree.h>

#include <bitset>
#include <forward_list>
#include <map>
#include <string>
#include <vector>

class Members : public TObject {
public:
   std::map<int, double> fMap;
   std::bitset<8> fFlags;
   TNamed fTarget;
   TRef fRef;
   TMap fTable;
   TBits fMask;
   TObject *fAny = nullptr;
   std::forward_list<int> fForward;
   TList fTagged;
   TList fPlain;
   TObjArray fArray;
   Double32_t fRanged = 0; //[-1,1,16]
   Float16_t fHalf = 0;    //[0,10,12]
   std::string fLabel;
   ~Members() override
   {
      delete fAny;
      fTagged.Delete();
      fPlain.Delete();
      fArray.Delete();
      fTable.DeleteAll();
   }
   ClassDefOverride(Members, 1)
};

#ifdef __ROOTCLING__
#pragma link C++ class Members+;
#pragma link C++ class std::forward_list<int>+;
#pragma link C++ class std::map<int, std::string>+;
#pragma link C++ class std::map<std::string, std::vector<float>>+;
#endif

void members_write(const char *path)
{
   // Uncompressed: diagnostics then carry file offsets.
   TFile f(path, "RECREATE", "", 0);
   Members m;
   m.fMap = {{1, 0.5}, {2, -1.25}, {7, 3.0}};
   m.fFlags = std::bitset<8>("10100101");
   m.fTarget.SetNameTitle("target", "the referenced object");
   m.fRef = &m.fTarget;
   m.fTable.Add(new TObjString("key"), new TObjString("value"));
   m.fTable.Add(new TObjString("other"), new TNamed("named", "a value"));
   m.fMask.SetBitNumber(3);
   m.fMask.SetBitNumber(9);
   auto any = new TBits(16);
   any->SetBitNumber(1);
   m.fAny = any;
   m.fForward = {3, 2, 1};
   m.fTagged.Add(new TObjString("a"), "first");
   m.fTagged.Add(new TObjString("b"), "second");
   m.fPlain.Add(new TObjString("c"));
   m.fPlain.Add(new TObjString("d"));
   m.fArray.SetName("array");
   m.fArray.AddAtAndExpand(new TObjString("e"), 0);
   m.fRanged = 0.5;
   m.fHalf = 2.5;
   m.fLabel = "members";
   m.Write("members");

   TTree t("t", "unsplit maps");
   std::map<int, std::string> ms;
   std::map<std::string, std::vector<float>> mv;
   t.Branch("ms", &ms, 32000, 0);
   t.Branch("mv", &mv, 32000, 0);
   for (int i = 0; i < 3; ++i) {
      ms.clear();
      mv.clear();
      for (int k = 0; k < i + 1; ++k) {
         ms[10 * i + k] = "s" + std::to_string(10 * i + k);
         std::vector<float> v;
         for (int j = 0; j < k; ++j)
            v.push_back(i + 0.5f * j);
         mv["k" + std::to_string(k)] = v;
      }
      t.Fill();
   }
   t.Write();
   f.Close();
}
